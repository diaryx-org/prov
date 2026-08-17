//! Manifests — one node standing for a whole directory of opaque files.
//!
//! An [attachment](crate::document::Document::is_attachment) gives *one* file
//! workspace-linked metadata by minting a sidecar beside it. That trade stops
//! working at scale: a directory of ten thousand photographs would mean ten
//! thousand sidecars, which is not an archive anyone can read, edit or sync.
//!
//! A **manifest** is the bulk form of the same idea. A node declares
//! `manifest: photos.manifest.yaml` — mutually exclusive with `content`, because
//! a node stands for one payload or for a set, never both — and that document is
//! a whole-file record store listing the files under a directory it names:
//!
//! ```yaml
//! # photos.manifest.yaml
//! title: Photos — manifest
//! root: photos/
//! files:
//!   - path: 2019/IMG_0001.jpg
//!     hash: sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae
//!   - path: 2019/IMG_0002.jpg
//!     hash: sha256:fcde2b2edba56bf408601fb721fe9b5c338d10ee429ea04fae5511b68fbf8fb9
//! ```
//!
//! Three properties fall out of that shape, and each is load-bearing:
//!
//! - **Rows are relative to `root`, not to the workspace.** Moving the covered
//!   directory rewrites one line (`root:`) rather than every row, which for ten
//!   thousand of them is the difference between a move and a rewrite.
//! - **`hash` is optional.** A manifest with no hashes is an inventory — what is
//!   supposed to be here — and one with hashes is that plus a fixity baseline.
//!   Hashing ten thousand files has a real cost, so it is a choice, not a tax.
//! - **The manifest is hashed by its node.** The sidecar's `content_hash` covers
//!   the manifest document's bytes exactly as an attachment's covers its
//!   payload's, which is what makes the per-file hashes trustworthy: tampering
//!   with a row means tampering with the file the node has already pinned.
//!
//! `root` claims the directory **completely** for opaque payloads: a file under
//! it that no row names is drift prov reports, which is the question a photo
//! archive actually has ("did something appear, did something vanish?") and the
//! one an open-ended list can never answer. Files prov *can* read as documents
//! are not claimed — they stay ordinary documents, linked and orphan-checked as
//! usual — so a manifest never shadows a document, and the "opacity is a role"
//! escape hatch (`attach --opaque`) stays a single-file affair.
//!
//! This module is the model and its serialization, which is all the read core
//! needs; the verbs that build, refresh and verify one are `prov`'s.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::meta::{Mapping, Value};

/// The node key naming a manifest document — the bulk counterpart of `content`,
/// and mutually exclusive with it.
pub const MANIFEST_KEY: &str = "manifest";

/// The manifest key naming the directory it covers, relative to the manifest
/// document's own directory.
pub const ROOT_KEY: &str = "root";

/// The manifest key holding the rows.
pub const FILES_KEY: &str = "files";

/// The per-row key naming the file, relative to [`ROOT_KEY`].
pub const PATH_KEY: &str = "path";

/// The per-row key holding the digest, spelled `sha256:<hex>` as
/// `prov_fixity::digest` produces it. Optional.
pub const HASH_KEY: &str = "hash";

/// The infix a manifest document's name carries, so a node's manifest is found
/// beside it by convention (`photos.yaml` ↔ `photos.manifest.yaml`) the way an
/// attachment's payload is.
pub const MANIFEST_INFIX: &str = "manifest";

/// One row: a covered file and, when the manifest carries a fixity baseline,
/// the digest of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    /// The file, relative to the manifest's `root` and normalized.
    pub path: PathBuf,
    /// `sha256:<hex>`, or `None` in an unhashed (inventory-only) manifest.
    pub hash: Option<String>,
}

/// A parsed manifest document: the directory it claims, and the files it says
/// are in it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    /// The covered directory as written, relative to the manifest document's
    /// own directory.
    pub root: String,
    /// The rows, sorted by [`path_sort_key`] when this library wrote them; a
    /// manifest read back off disk keeps whatever order it was written in.
    pub files: Vec<ManifestEntry>,
}

impl Manifest {
    /// Read a manifest out of a loaded document's metadata.
    ///
    /// Strict about the two things a reader must be able to trust — a `root` it
    /// can resolve and rows that name a path — and permissive about everything
    /// else, since a manifest is a document a person may edit: an unknown key is
    /// carried past, and a row that is not a mapping with a `path` is refused
    /// rather than silently dropped, because a dropped row reads as "that file
    /// was never claimed" and would turn a damaged manifest into a clean report.
    pub fn from_meta(meta: &Value) -> Result<Self> {
        let root = meta
            .get(ROOT_KEY)
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Structure(format!("manifest has no `{ROOT_KEY}`")))?
            .to_string();
        let rows = match meta.get(FILES_KEY) {
            // A manifest over an empty directory has no rows at all, which is a
            // legitimate state (`files:` written as null, or absent).
            None => &[][..],
            Some(Value::Null) => &[][..],
            Some(value) => value.as_sequence().ok_or_else(|| {
                Error::Structure(format!("manifest `{FILES_KEY}` must be a sequence"))
            })?,
        };
        let mut files = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let path = row
                .get(PATH_KEY)
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Structure(format!("manifest row {i} has no `{PATH_KEY}`")))?;
            if crate::link::escapes_root(path) {
                return Err(Error::Structure(format!(
                    "manifest row {i} (`{path}`) climbs outside the manifest's root"
                )));
            }
            files.push(ManifestEntry {
                path: crate::link::normalize(path),
                hash: row
                    .get(HASH_KEY)
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        Ok(Manifest { root, files })
    }

    /// The manifest as a whole-file document mapping, `title` first — the shape
    /// [`from_meta`](Self::from_meta) reads back.
    ///
    /// Rows are emitted in the order they are held; a manifest this library
    /// builds is sorted by [`path_sort_key`] first, so the file a person opens
    /// reads like a directory listing rather than a walk order.
    pub fn to_mapping(&self, title: &str) -> Mapping {
        let mut map = Mapping::new();
        map.insert("title".into(), Value::String(title.to_string()));
        map.insert(ROOT_KEY.into(), Value::String(self.root.clone()));
        map.insert(
            FILES_KEY.into(),
            Value::Sequence(
                self.files
                    .iter()
                    .map(|entry| {
                        let mut row = Mapping::new();
                        row.insert(PATH_KEY.into(), Value::String(slash_path(&entry.path)));
                        if let Some(hash) = &entry.hash {
                            row.insert(HASH_KEY.into(), Value::String(hash.clone()));
                        }
                        Value::Mapping(row)
                    })
                    .collect(),
            ),
        );
        map
    }

    /// The covered directory, workspace-relative, given where the manifest
    /// document itself lives.
    pub fn covered_root(&self, manifest_doc: &Path) -> PathBuf {
        crate::link::resolve(manifest_doc, &self.root)
    }

    /// [`covered_root`](Self::covered_root), refusing one that lands outside the
    /// workspace.
    ///
    /// The check cannot live in [`from_meta`](Self::from_meta), which has no
    /// path to resolve against: `root` is relative to the *manifest's* directory,
    /// so `../photos/` is an ordinary, correct value for a manifest one
    /// directory down — and is exactly what a rename writes. Only the resolved
    /// path can say whether a workspace boundary was crossed.
    pub fn checked_root(&self, manifest_doc: &Path) -> Result<PathBuf> {
        let root = self.covered_root(manifest_doc);
        if crate::link::escapes_root(&root) {
            return Err(Error::Structure(format!(
                "manifest `{ROOT_KEY}: {}` climbs outside the workspace",
                self.root
            )));
        }
        Ok(root)
    }

    /// A row's file, workspace-relative — the covered root joined with the row's
    /// own relative path.
    pub fn file_path(&self, manifest_doc: &Path, entry: &ManifestEntry) -> PathBuf {
        crate::link::normalize(self.covered_root(manifest_doc).join(&entry.path))
    }

    /// Sort the rows into the order this library writes them (§ the module doc:
    /// byte-wise on the `/`-joined path), so two manifests built from the same
    /// directory are the same bytes.
    pub fn sort(&mut self) {
        self.files.sort_by(|a, b| {
            path_sort_key(&a.path)
                .cmp(&path_sort_key(&b.path))
                .then_with(|| a.hash.cmp(&b.hash))
        });
    }

    /// Whether every row carries a digest — the difference between an inventory
    /// and a fixity baseline. An empty manifest counts as hashed: it makes the
    /// same promise about all zero of its files.
    pub fn is_hashed(&self) -> bool {
        self.files.iter().all(|entry| entry.hash.is_some())
    }
}

/// How a manifest's rows and a directory listing disagree: `(missing, extra)` —
/// rows whose file is not on disk, and opaque files under the root that no row
/// claims. Both relative to the root, both sorted.
///
/// The completeness rule in one place, because it is the whole meaning of
/// `root`: the manifest claims that directory entirely, so a file it does not
/// name is drift and not merely an omission. `check` and the `manifest` report
/// both ask this question and must not be able to answer it differently.
pub fn diff(listed: &[ManifestEntry], on_disk: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let rows: std::collections::BTreeSet<&Path> = listed.iter().map(|e| e.path.as_path()).collect();
    let disk: std::collections::BTreeSet<&Path> = on_disk.iter().map(PathBuf::as_path).collect();
    (
        rows.difference(&disk).map(|p| p.to_path_buf()).collect(),
        disk.difference(&rows).map(|p| p.to_path_buf()).collect(),
    )
}

/// A path spelled with `/` separators regardless of host platform — what a
/// manifest row holds, so a manifest written on Windows and one written on Linux
/// describe the same directory identically.
pub fn slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The row sort key: byte-wise ascending on the `/`-joined UTF-8 path, **not**
/// `Path::cmp`, which orders component-wise and disagrees with it (`a.jpg` vs
/// `a/b.jpg`: joined, `.` sorts before `/`; component-wise, the bare `a` is a
/// prefix of `a.jpg` and so sorts first). The rows end up serialized as those
/// joined strings, so the joined order is the one a reader sees.
pub fn path_sort_key(path: &Path) -> String {
    slash_path(path)
}

/// Where a node's manifest document sits: beside it, with `manifest` infixed
/// before the metadata extension (`photos.yaml` → `photos.manifest.yaml`).
///
/// The infix rather than a replaced extension, for the same reason an attachment
/// sidecar appends rather than replaces: the node and its manifest are both
/// whole-file metadata documents in the same format, so a convention that only
/// swapped the extension would name the node itself.
pub fn manifest_sibling(node: &Path) -> PathBuf {
    let stem = node
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let ext = node.extension().and_then(|e| e.to_str()).unwrap_or("yaml");
    node.with_file_name(format!("{stem}.{MANIFEST_INFIX}.{ext}"))
}

/// Every path that could be the node covering the directory `dir`, under the
/// `<dir>.<ext>` convention — the probe half of the reverse lookup, exactly as
/// [`sidecar_candidates`](crate::graph::sidecar_candidates) is for a payload.
/// The node's `manifest` pointer, and that manifest's `root`, are what confirm a
/// hit.
pub fn manifest_node_candidates(dir: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    crate::graph::sidecar_candidates(dir)
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Manifest> {
        let map = crate::meta::parse_mapping(text, fig::Format::Yaml).unwrap();
        Manifest::from_meta(&Value::Mapping(map))
    }

    #[test]
    fn reads_rows_with_and_without_hashes() {
        let m = parse(
            "title: Photos\nroot: photos/\nfiles:\n\
             - path: a.jpg\n  hash: sha256:abc\n\
             - path: sub/b.jpg\n",
        )
        .unwrap();
        assert_eq!(m.root, "photos/");
        assert_eq!(m.files.len(), 2);
        assert_eq!(m.files[0].hash.as_deref(), Some("sha256:abc"));
        assert_eq!(m.files[1].path, PathBuf::from("sub/b.jpg"));
        assert!(!m.is_hashed(), "one row carries no digest");
    }

    #[test]
    fn an_empty_manifest_is_legal_and_a_rootless_one_is_not() {
        assert!(parse("root: photos/\n").unwrap().files.is_empty());
        assert!(parse("root: photos/\nfiles:\n").unwrap().files.is_empty());
        assert!(parse("files:\n- path: a.jpg\n").is_err(), "no root");
    }

    #[test]
    fn a_row_without_a_path_is_refused_rather_than_dropped() {
        // Dropping it would read as "that file was never claimed", turning a
        // damaged manifest into a clean report — the one failure mode a fixity
        // record must not have.
        let err = parse("root: photos/\nfiles:\n- hash: sha256:abc\n").unwrap_err();
        assert!(err.to_string().contains("path"), "{err}");
    }

    #[test]
    fn neither_root_nor_row_may_climb_out_of_the_workspace() {
        // A row climbs out of the manifest's own root, which nothing resolves
        // away, so it is refused at parse time.
        assert!(parse("root: photos/\nfiles:\n- path: ../../etc/passwd\n").is_err());

        // A `root` can only be judged once resolved: `../photos/` is correct for
        // a manifest one directory down (and is what a rename writes), while the
        // same spelling from the workspace root is an escape.
        let m = parse("root: ../photos/\n").unwrap();
        assert!(
            m.checked_root(Path::new("albums/trip.manifest.yaml"))
                .is_ok()
        );
        assert!(m.checked_root(Path::new("trip.manifest.yaml")).is_err());
    }

    #[test]
    fn rows_resolve_against_the_root_not_the_workspace() {
        let m = parse("root: photos/\nfiles:\n- path: 2019/a.jpg\n").unwrap();
        let doc = Path::new("albums/trip.manifest.yaml");
        assert_eq!(m.covered_root(doc), PathBuf::from("albums/photos"));
        assert_eq!(
            m.file_path(doc, &m.files[0]),
            PathBuf::from("albums/photos/2019/a.jpg")
        );
    }

    #[test]
    fn rows_sort_byte_wise_on_the_joined_path() {
        // `a.jpg` before `a/b.jpg` — the order the serialized file shows, which
        // `Path::cmp` would invert.
        let mut m = Manifest {
            root: "photos/".into(),
            files: vec![
                ManifestEntry {
                    path: PathBuf::from("a/b.jpg"),
                    hash: None,
                },
                ManifestEntry {
                    path: PathBuf::from("a.jpg"),
                    hash: None,
                },
            ],
        };
        m.sort();
        assert_eq!(m.files[0].path, PathBuf::from("a.jpg"));
    }

    #[test]
    fn round_trips_through_a_mapping() {
        let m = parse("root: photos/\nfiles:\n- path: a.jpg\n  hash: sha256:abc\n").unwrap();
        let text =
            crate::meta::serialize_mapping(&m.to_mapping("Photos — manifest"), fig::Format::Yaml)
                .unwrap();
        assert!(text.contains("root: photos/"), "{text}");
        assert_eq!(parse(&text).unwrap(), m);
    }

    #[test]
    fn the_manifest_sits_beside_its_node_without_naming_it() {
        assert_eq!(
            manifest_sibling(Path::new("albums/photos.yaml")),
            PathBuf::from("albums/photos.manifest.yaml")
        );
        assert_ne!(
            manifest_sibling(Path::new("photos.yaml")),
            PathBuf::from("photos.yaml")
        );
    }
}
