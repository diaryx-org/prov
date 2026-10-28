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
//! - **A row may say more about its file.** Any key beside `path` and `hash`
//!   is a field of that one file — a caption, a date, a title someone wrote —
//!   kept as written. It is data, not a link: the graph does not read links
//!   in a manifest, so a link written on a row would be one no rename
//!   rewrites and no `check` reports broken.
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
//! The one exception is opt-in and written in the manifest itself:
//! `verbatim: true` claims **every** file under `root`, documents included.
//! It is for a directory whose files are one artifact to be carried byte for
//! byte — a built web app, whose `index.html` is part of the build and not a
//! page anyone writes — so the promise is visible where it is made, as
//! `attach --opaque`'s is in its sidecar.
//!
//! The other narrowing is written in the manifest too: `exclude:` lists
//! patterns, in a subset of gitignore's syntax, for files under `root` that
//! belong to whatever else keeps the directory — a NAS's thumbnail folders, a
//! recycle bin, a desktop's index files — and are neither the archive nor
//! drift. An excluded directory is not walked at all. See
//! [`Manifest::excludes`] for the syntax.
//!
//! And a manifest may say its files are kept somewhere else: `held:` names the
//! machine that holds them (`by`) and, optionally, where to ask it for one
//! (`at`). On a device without the covered directory, a held manifest's rows
//! are elsewhere rather than missing — a photo archive on a NAS, described in a
//! library every device carries, whose files only the NAS has. Where the
//! directory is present nothing changes. See [`Held`].
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

/// The manifest key that, set to `true`, claims every file under `root` —
/// documents included — instead of the opaque payloads alone.
pub const VERBATIM_KEY: &str = "verbatim";

/// The manifest key listing patterns for files under `root` the manifest does
/// not claim — see [`Manifest::excludes`].
pub const EXCLUDE_KEY: &str = "exclude";

/// The manifest key saying who keeps the covered files — see [`Held`].
pub const HELD_KEY: &str = "held";

/// The manifest key holding the rows.
pub const FILES_KEY: &str = "files";

/// The per-row key naming the file, relative to [`ROOT_KEY`].
pub const PATH_KEY: &str = "path";

/// The per-row key holding the digest, spelled `sha256:<hex>` as
/// [`fixity::digest`](crate::fixity::digest) produces it. Optional.
pub const HASH_KEY: &str = "hash";

/// The infix a manifest document's name carries, so a node's manifest is found
/// beside it by convention (`photos.yaml` ↔ `photos.manifest.yaml`) the way an
/// attachment's payload is.
pub const MANIFEST_INFIX: &str = "manifest";

/// One row: a covered file, the digest of its bytes when the manifest carries
/// a fixity baseline, and whatever else the row says about that file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ManifestEntry {
    /// The file, relative to the manifest's `root` and normalized.
    pub path: PathBuf,
    /// `sha256:<hex>`, or `None` in an unhashed (inventory-only) manifest.
    pub hash: Option<String>,
    /// Every other key on the row, in the order written: fields of this one
    /// file. Never `path` or `hash`.
    pub fields: Mapping,
}

/// Who keeps a manifest's files, when they are not on every device that
/// carries the manifest ([`HELD_KEY`]).
///
/// Written as `held: {by: Harris NAS, at: https://nas.example/}`. Neither is a
/// secret: who may ask is the keeper's to decide, never settled by knowing the
/// address.
///
/// It changes what an absent root means and nothing else. Without it, a
/// covered directory that is not there is a broken link and every row is
/// missing; with it, the rows are held elsewhere, `check` says nothing about
/// them, and the verbs that would rebuild the rows from the directory refuse
/// rather than empty the manifest. Where the directory is present — on the
/// keeper itself — the manifest is checked exactly as an unheld one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Held {
    /// The keeper, named for a person: `Harris NAS`.
    pub by: String,
    /// Where a device asks the keeper for a file, if it can be asked at all.
    pub at: Option<String>,
}

impl Held {
    fn from_value(value: &Value) -> Result<Self> {
        let refuse = || {
            Error::Structure(format!(
                "manifest `{HELD_KEY}` must be `{{by: <name>, at: <address>}}`"
            ))
        };
        let map = value.as_mapping().ok_or_else(refuse)?;
        let mut by = None;
        let mut at = None;
        for (key, value) in map {
            match key.as_str() {
                "by" => by = Some(value.as_str().ok_or_else(refuse)?),
                "at" => match value {
                    Value::Null => {}
                    value => at = Some(value.as_str().ok_or_else(refuse)?.to_string()),
                },
                _ => return Err(refuse()),
            }
        }
        let by = by
            .map(str::trim)
            .filter(|by| !by.is_empty())
            .ok_or_else(refuse)?;
        Ok(Held {
            by: by.to_string(),
            at,
        })
    }

    fn to_value(&self) -> Value {
        let mut map = Mapping::new();
        map.insert("by".into(), Value::String(self.by.clone()));
        if let Some(at) = &self.at {
            map.insert("at".into(), Value::String(at.clone()));
        }
        Value::Mapping(map)
    }
}

/// A parsed manifest document: the directory it claims, and the files it says
/// are in it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Manifest {
    /// The covered directory as written, relative to the manifest document's
    /// own directory.
    pub root: String,
    /// The rows, sorted by [`path_sort_key`] when this library wrote them; a
    /// manifest read back off disk keeps whatever order it was written in.
    pub files: Vec<ManifestEntry>,
    /// Whether the manifest claims every file under `root`, documents
    /// included ([`VERBATIM_KEY`]), rather than the opaque payloads alone.
    pub verbatim: bool,
    /// Patterns for files under `root` that the manifest does not claim
    /// ([`EXCLUDE_KEY`]), as written. See [`excludes`](Self::excludes).
    pub exclude: Vec<String>,
    /// Who keeps the files, when not every device has them ([`HELD_KEY`]).
    pub held: Option<Held>,
}

impl Manifest {
    /// Read a manifest out of a loaded document's metadata.
    ///
    /// Strict about the two things a reader must be able to trust — a `root` it
    /// can resolve and rows that name a path — and permissive about everything
    /// else, since a manifest is a document a person may edit: any other key on
    /// a row is kept as one of its [`fields`](ManifestEntry::fields), and a row
    /// that is not a mapping with a `path` is refused rather than silently
    /// dropped, because a dropped row reads as "that file was never claimed"
    /// and would turn a damaged manifest into a clean report.
    pub fn from_meta(meta: &Value) -> Result<Self> {
        let root = meta
            .get(ROOT_KEY)
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Structure(format!("manifest has no `{ROOT_KEY}`")))?
            .to_string();
        // Refused rather than read as `false` when it is not a boolean: the
        // flag decides which files the manifest claims, and a misread one
        // would turn every claimed document into drift, or the reverse.
        let verbatim = match meta.get(VERBATIM_KEY) {
            None | Some(Value::Null) => false,
            Some(value) => value.as_bool().ok_or_else(|| {
                Error::Structure(format!("manifest `{VERBATIM_KEY}` must be true or false"))
            })?,
        };
        let exclude = match meta.get(EXCLUDE_KEY) {
            None | Some(Value::Null) => Vec::new(),
            Some(value) => {
                let items = value.as_sequence().ok_or_else(|| {
                    Error::Structure(format!(
                        "manifest `{EXCLUDE_KEY}` must be a list of patterns"
                    ))
                })?;
                let mut patterns = Vec::with_capacity(items.len());
                for item in items {
                    let pattern = item.as_str().ok_or_else(|| {
                        Error::Structure(format!(
                            "manifest `{EXCLUDE_KEY}` must be a list of patterns"
                        ))
                    })?;
                    check_pattern(pattern)?;
                    patterns.push(pattern.to_string());
                }
                patterns
            }
        };
        let held = match meta.get(HELD_KEY) {
            None | Some(Value::Null) => None,
            Some(value) => Some(Held::from_value(value)?),
        };
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
            let fields = row
                .as_mapping()
                .into_iter()
                .flatten()
                .filter(|(key, _)| *key != PATH_KEY && *key != HASH_KEY)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            files.push(ManifestEntry {
                path: crate::link::normalize(path),
                hash: row
                    .get(HASH_KEY)
                    .and_then(Value::as_str)
                    .map(str::to_string),
                fields,
            });
        }
        Ok(Manifest {
            root,
            files,
            verbatim,
            exclude,
            held,
        })
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
        if self.verbatim {
            map.insert(VERBATIM_KEY.into(), Value::Bool(true));
        }
        if let Some(held) = &self.held {
            map.insert(HELD_KEY.into(), held.to_value());
        }
        if !self.exclude.is_empty() {
            map.insert(
                EXCLUDE_KEY.into(),
                Value::Sequence(self.exclude.iter().cloned().map(Value::String).collect()),
            );
        }
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
                        row.extend(entry.fields.clone());
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

    /// Whether the file at `rel`, relative to the covered root, is one this
    /// manifest claims: any file when it is [`verbatim`](Self::verbatim), an
    /// opaque payload otherwise.
    pub fn claims(&self, rel: &Path) -> bool {
        self.verbatim || crate::document::is_opaque_payload(rel)
    }

    /// Whether one of the manifest's [`exclude`](Self::exclude) patterns
    /// leaves out the entry at `rel`, relative to the covered root — a
    /// directory when `is_dir`, a file otherwise. The entry alone is judged,
    /// not the directories above it: a walk never enters an excluded
    /// directory, so it never asks about what is inside one.
    /// [`excludes_file`](Self::excludes_file) asks about all of them.
    ///
    /// The syntax is a subset of gitignore's, so a list can be copied from
    /// one: a pattern with no `/` but a trailing one matches an entry's
    /// *name* at any depth (`@eaDir`, `*@SynoEAStream`, `.DS_Store`); one
    /// with a `/` before its end is anchored to the root (`/#recycle`,
    /// `2019/raw`); a trailing `/` matches directories only. `*` matches
    /// within one name, `?` one character, `**` as a whole name any number of
    /// directories, and `\` makes the next character literal. Negation
    /// (`!`) and character classes are refused when the manifest is read
    /// rather than misread.
    pub fn excludes(&self, rel: &Path, is_dir: bool) -> bool {
        excluded_by(&self.exclude, rel, is_dir)
    }

    /// Whether the file at `rel` is left out, by a pattern naming it or one
    /// naming a directory above it — the question for a file named from
    /// outside a walk.
    pub fn excludes_file(&self, rel: &Path) -> bool {
        let mut prefix = PathBuf::new();
        let mut segments = rel.iter().peekable();
        while let Some(segment) = segments.next() {
            prefix.push(segment);
            let is_dir = segments.peek().is_some();
            if self.excludes(&prefix, is_dir) {
                return true;
            }
        }
        false
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

/// [`Manifest::excludes`] over a bare list of patterns, for a walk that holds
/// the patterns but not the manifest.
pub fn excluded_by(patterns: &[String], rel: &Path, is_dir: bool) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let segments: Vec<String> = rel
        .iter()
        .map(|segment| segment.to_string_lossy().into_owned())
        .collect();
    patterns
        .iter()
        .any(|pattern| pattern_matches(pattern, &segments, is_dir))
}

/// Refuse what [`Manifest::excludes`] does not read, so a pattern copied from
/// a gitignore that means more there is never quietly taken to mean less.
pub fn check_pattern(pattern: &str) -> Result<()> {
    let refuse = |why: &str| {
        Err(Error::Structure(format!(
            "manifest `{EXCLUDE_KEY}` pattern `{pattern}`: {why}"
        )))
    };
    let body = pattern.trim_end_matches('/');
    if body.trim_start_matches('/').is_empty() {
        return refuse("it names nothing");
    }
    if pattern.starts_with('!') {
        return refuse("negation is not supported");
    }
    let mut escaped = false;
    for character in pattern.chars() {
        match (escaped, character) {
            (true, _) => escaped = false,
            (false, '\\') => escaped = true,
            (false, '[' | ']') => return refuse("character classes are not supported"),
            _ => {}
        }
    }
    if body.split('/').any(|segment| segment == "..") {
        return refuse("it climbs outside the manifest's root");
    }
    Ok(())
}

/// Whether `pattern` matches the entry whose path below the root is
/// `segments` — see [`Manifest::excludes`].
fn pattern_matches(pattern: &str, segments: &[String], is_dir: bool) -> bool {
    let dir_only = pattern.ends_with('/');
    if dir_only && !is_dir {
        return false;
    }
    let body = pattern.trim_end_matches('/');
    let anchored = body.contains('/');
    let body = body.trim_start_matches('/');
    if anchored {
        let parts: Vec<&str> = body.split('/').filter(|part| !part.is_empty()).collect();
        segments_match(&parts, segments)
    } else {
        segments.last().is_some_and(|name| name_matches(body, name))
    }
}

/// Whether the pattern segments `parts` match the path `segments` whole, `**`
/// standing for any number of them.
fn segments_match(parts: &[&str], segments: &[String]) -> bool {
    match parts.split_first() {
        None => segments.is_empty(),
        Some((&"**", rest)) => {
            (0..=segments.len()).any(|skip| segments_match(rest, &segments[skip..]))
        }
        Some((part, rest)) => segments
            .split_first()
            .is_some_and(|(name, tail)| name_matches(part, name) && segments_match(rest, tail)),
    }
}

/// Whether the one-name glob `pattern` matches `name`: `*` any run of
/// characters, `?` one, `\` the next character literally.
fn name_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    glob(&pattern, &name)
}

fn glob(pattern: &[char], name: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => (0..=name.len()).any(|skip| glob(rest, &name[skip..])),
        Some(('?', rest)) => !name.is_empty() && glob(rest, &name[1..]),
        Some(('\\', rest)) if !rest.is_empty() => {
            name.first() == Some(&rest[0]) && glob(&rest[1..], &name[1..])
        }
        Some((literal, rest)) => name.first() == Some(literal) && glob(rest, &name[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Manifest> {
        let map = crate::meta::parse_mapping(text, crate::MetaFormat::Yaml).unwrap();
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
                    ..Default::default()
                },
                ManifestEntry {
                    path: PathBuf::from("a.jpg"),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        m.sort();
        assert_eq!(m.files[0].path, PathBuf::from("a.jpg"));
    }

    #[test]
    fn verbatim_is_opt_in_and_must_be_a_boolean() {
        let plain = parse("root: app/\n").unwrap();
        assert!(!plain.verbatim);
        assert!(!plain.claims(Path::new("index.html")));
        assert!(plain.claims(Path::new("index-abc.js")));

        let verbatim = parse("root: app/\nverbatim: true\n").unwrap();
        assert!(verbatim.verbatim);
        assert!(verbatim.claims(Path::new("index.html")));

        assert!(parse("root: app/\nverbatim: yes please\n").is_err());
    }

    #[test]
    fn a_verbatim_manifest_round_trips_and_a_plain_one_writes_no_flag() {
        let text = |m: &Manifest| {
            crate::meta::serialize_mapping(&m.to_mapping("App — manifest"), crate::MetaFormat::Yaml)
                .unwrap()
        };
        let plain = parse("root: app/\nfiles:\n- path: a.js\n").unwrap();
        assert!(!text(&plain).contains("verbatim"), "{}", text(&plain));
        let verbatim = parse("root: app/\nverbatim: true\nfiles:\n- path: index.html\n").unwrap();
        assert!(
            text(&verbatim).contains("verbatim: true"),
            "{}",
            text(&verbatim)
        );
        assert_eq!(parse(&text(&verbatim)).unwrap(), verbatim);
    }

    #[test]
    fn round_trips_through_a_mapping() {
        let m = parse("root: photos/\nfiles:\n- path: a.jpg\n  hash: sha256:abc\n").unwrap();
        let text = crate::meta::serialize_mapping(
            &m.to_mapping("Photos — manifest"),
            crate::MetaFormat::Yaml,
        )
        .unwrap();
        assert!(text.contains("root: photos/"), "{text}");
        assert_eq!(parse(&text).unwrap(), m);
    }

    #[test]
    fn a_row_keeps_what_else_it_says_about_its_file() {
        let m = parse(
            "root: photos/\nfiles:\n\
             - path: a.jpg\n  hash: sha256:abc\n  title: Mum at the lake\n  date: 2019-07-04\n\
             - path: b.jpg\n",
        )
        .unwrap();
        let keys: Vec<&str> = m.files[0].fields.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["title", "date"],
            "in the order written, path and hash apart"
        );
        assert!(m.files[1].fields.is_empty());

        let text = crate::meta::serialize_mapping(
            &m.to_mapping("Photos — manifest"),
            crate::MetaFormat::Yaml,
        )
        .unwrap();
        assert!(text.contains("title: Mum at the lake"), "{text}");
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

    #[test]
    fn exclude_round_trips_and_is_left_out_when_empty() {
        let text = |m: &Manifest| {
            crate::meta::serialize_mapping(
                &m.to_mapping("Photos — manifest"),
                crate::MetaFormat::Yaml,
            )
            .unwrap()
        };
        let plain = parse("root: photos/\n").unwrap();
        assert!(plain.exclude.is_empty());
        assert!(!text(&plain).contains("exclude"), "{}", text(&plain));
        let excluding =
            parse("root: photos/\nexclude:\n- '@eaDir/'\n- '*@SynoEAStream'\n").unwrap();
        assert_eq!(excluding.exclude, ["@eaDir/", "*@SynoEAStream"]);
        assert_eq!(parse(&text(&excluding)).unwrap(), excluding);
    }

    #[test]
    fn held_round_trips_and_is_left_out_when_absent() {
        let text = |m: &Manifest| {
            crate::meta::serialize_mapping(
                &m.to_mapping("Photo — manifest"),
                crate::MetaFormat::Yaml,
            )
            .unwrap()
        };
        let plain = parse("root: photo/\n").unwrap();
        assert_eq!(plain.held, None);
        assert!(!text(&plain).contains("held"), "{}", text(&plain));
        let held =
            parse("root: photo/\nheld:\n  by: Harris NAS\n  at: https://nas.example/\n").unwrap();
        assert_eq!(
            held.held,
            Some(Held {
                by: "Harris NAS".into(),
                at: Some("https://nas.example/".into()),
            })
        );
        assert_eq!(parse(&text(&held)).unwrap(), held);
        let nameless = parse("root: photo/\nheld: {by: Harris NAS}\n").unwrap();
        assert_eq!(nameless.held.unwrap().at, None);
    }

    #[test]
    fn a_held_it_cannot_read_is_refused() {
        for bad in [
            "held: Harris NAS\n",
            "held: {at: https://nas.example/}\n",
            "held: {by: '  '}\n",
            "held: {by: 3}\n",
            "held: {by: NAS, where: attic}\n",
        ] {
            let text = format!("root: photo/\n{bad}");
            assert!(parse(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_exclude_it_cannot_read_is_refused() {
        for bad in [
            "exclude: '@eaDir'\n",
            "exclude:\n- 3\n",
            "exclude:\n- '!keep.jpg'\n",
            "exclude:\n- '[ab].jpg'\n",
            "exclude:\n- ../up\n",
            "exclude:\n- /\n",
        ] {
            assert!(parse(&format!("root: photos/\n{bad}")).is_err(), "{bad}");
        }
        // An escaped bracket is a literal one, and fine.
        assert!(parse("root: photos/\nexclude:\n- 'x\\[1\\].jpg'\n").is_ok());
    }

    #[test]
    fn exclude_patterns_read_as_gitignore_reads_them() {
        let m = |patterns: &[&str]| Manifest {
            root: "photos/".into(),
            exclude: patterns.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        };
        let p = Path::new;

        // No slash: a name at any depth, file or directory.
        let name = m(&["@eaDir", "*@SynoEAStream", "Thumbs.db"]);
        assert!(name.excludes(p("@eaDir"), true));
        assert!(name.excludes(p("2019/07/@eaDir"), true));
        assert!(name.excludes(p("2019/a.jpg@SynoEAStream"), false));
        assert!(name.excludes(p("Thumbs.db"), false));
        assert!(!name.excludes(p("2019/a.jpg"), false));
        assert!(!name.excludes(p("@eaDirectory"), true));

        // A trailing slash: directories only.
        let dirs = m(&["raw/"]);
        assert!(dirs.excludes(p("2019/raw"), true));
        assert!(!dirs.excludes(p("2019/raw"), false));

        // A slash before the end: anchored to the root.
        let anchored = m(&["/#recycle", "2019/raw", "**/cache/*.tmp"]);
        assert!(anchored.excludes(p("#recycle"), true));
        assert!(!anchored.excludes(p("2019/#recycle"), true));
        assert!(anchored.excludes(p("2019/raw"), true));
        assert!(!anchored.excludes(p("2020/2019/raw"), true));
        assert!(anchored.excludes(p("cache/a.tmp"), false));
        assert!(anchored.excludes(p("a/b/cache/x.tmp"), false));
        assert!(!anchored.excludes(p("a/b/cache/x.jpg"), false));

        // `?` is one character, `\` makes the next literal.
        let globs = m(&["IMG_????.HEIC", "star\\*.jpg"]);
        assert!(globs.excludes(p("IMG_0001.HEIC"), false));
        assert!(!globs.excludes(p("IMG_01.HEIC"), false));
        assert!(globs.excludes(p("star*.jpg"), false));
        assert!(!globs.excludes(p("starry.jpg"), false));

        // A file named from outside a walk is out when any directory above it is.
        assert!(name.excludes_file(p("2019/@eaDir/a.jpg/THUMB.jpg")));
        assert!(!name.excludes_file(p("2019/a.jpg")));
        assert!(!dirs.excludes_file(p("2019/raw")));
        assert!(dirs.excludes_file(p("2019/raw/a.dng")));
    }
}
