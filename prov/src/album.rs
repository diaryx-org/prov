//! Albums: a selection from a manifest, by hash.
//!
//! The record shape is [`prov_graph::album`]; this module is the verbs.
//!
//! - [`album`](Workspace::album): what an album picks, each pick joined to its
//!   source's row, and the picks the source no longer lists.
//! - [`start_album`](Workspace::start_album): make a document an album of a
//!   manifest node, writing `picks_from` as the workspace writes any link.
//! - [`pick`](Workspace::pick) and [`unpick`](Workspace::unpick): add and take
//!   away photographs, named by the source's path for them or by digest.
//!
//! None of them reads a photograph. A pick is a row's digest, and the rows are
//! what the album's device has, whether or not the archive is here.

use std::path::{Path, PathBuf};

use crate::identity::{IdentityPolicy, Trigger};
use crate::workspace::Workspace;
use prov_graph::album::{Album, PICKS_FROM_KEY, PICKS_KEY, Pick, is_sha256};
use prov_graph::error::{Error, Result};
use prov_graph::fs::ReadStorage;
use prov_graph::graph::album::{AlbumSource, SourceError};
use prov_graph::index::IdIndex;
use prov_graph::link;
use prov_graph::manifest::ManifestEntry;
use prov_graph::meta::Value;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

/// An album as it stands against its source.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumView {
    /// The album as written.
    pub album: Album,
    /// The manifest it picks from.
    pub source: AlbumSource,
    /// Each pick the source still lists, in the album's order, with the
    /// source's row for it.
    pub picked: Vec<(Pick, ManifestEntry)>,
    /// The picks the source no longer lists.
    pub missing: Vec<Pick>,
}

/// What a [`pick`](Workspace::pick) or [`unpick`](Workspace::unpick) did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PickUpdate {
    /// The digests added or taken away.
    pub changed: Vec<String>,
    /// The digests asked for that were already as asked: picked already, for
    /// `pick`; not picked, for `unpick`.
    pub unchanged: Vec<String>,
    /// How many picks the album has now.
    pub total: usize,
}

fn source_error(node: &Path, album: &Album, error: SourceError) -> Error {
    match error {
        SourceError::Unresolved => Error::Structure(format!(
            "{}: `{PICKS_FROM_KEY}` ({}) links to nothing in this workspace",
            node.display(),
            album.picks_from
        )),
        SourceError::Malformed(e) => Error::Structure(format!("{}: {e}", node.display())),
    }
}

impl<FS: ReadStorage, Id, Ix: IdIndex> Workspace<FS, Id, Ix> {
    /// The album `node` declares, read against its source. `None` when `node`
    /// is not an album.
    pub async fn album(&self, node: &Path) -> Result<Option<AlbumView>> {
        let Some(album) = self.graph().album_of(node).await? else {
            return Ok(None);
        };
        let source = self
            .graph()
            .album_source(node, &album)
            .await
            .map_err(|e| source_error(node, &album, e))?;
        let rows: std::collections::HashMap<&str, &ManifestEntry> = source
            .manifest
            .files
            .iter()
            .filter_map(|row| Some((row.hash.as_deref()?, row)))
            .collect();
        let mut picked = Vec::new();
        let mut missing = Vec::new();
        for pick in &album.picks {
            match rows.get(pick.hash.as_str()) {
                Some(row) => picked.push((pick.clone(), (*row).clone())),
                None => missing.push(pick.clone()),
            }
        }
        Ok(Some(AlbumView {
            album,
            source,
            picked,
            missing,
        }))
    }
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Make the document at `node` an album of the manifest node `source`.
    ///
    /// `picks_from` is written as the workspace writes a link (by id where it
    /// links by id), and `picks` is started empty unless the document already
    /// has picks, which are kept: pointing an album at a new copy of its
    /// archive should not lose what it chose. Refused when `source` is not a
    /// manifest node with hashed rows, or when `node` is one itself.
    pub async fn start_album(&mut self, node: &Path, source: &Path) -> Result<()> {
        let node = link::normalize(node);
        let source = link::normalize(source);
        let (_, source_manifest) = self.manifest_of(&source).await?.ok_or_else(|| {
            Error::Structure(format!("{} is not a manifest node", source.display()))
        })?;
        if !source_manifest.files.is_empty()
            && source_manifest.files.iter().all(|row| row.hash.is_none())
        {
            return Err(Error::Structure(format!(
                "{} lists no hashes, so nothing in it can be picked",
                source.display()
            )));
        }
        let (text, doc) = self.load(&node).await?;
        if doc.manifest_attr().is_some() {
            return Err(Error::Structure(format!(
                "{} is a manifest node: a node covers a directory or picks from one, not both",
                node.display()
            )));
        }
        let title = self
            .document(&source)
            .await?
            .meta
            .get("title")
            .and_then(prov_graph::title::title_text)
            .unwrap_or_else(|| link::path_to_title(&source));
        let mut cs = self.change();
        let style = self.reference_style();
        let id = if style.registers() && self.identity().registration().fires_on(Trigger::Link) {
            Some(self.register(&source, Trigger::Link).await?)
        } else {
            None
        };
        let target = link::format_reference(style, &node, &source, id.as_ref(), &title);
        let mut text = prov_store::edit::set_meta_in_text(
            &text,
            doc.carrier,
            PICKS_FROM_KEY,
            &Value::String(target),
        )?;
        if doc.meta.get(PICKS_KEY).is_none() {
            text = prov_store::edit::set_meta_in_text(
                &text,
                doc.carrier,
                PICKS_KEY,
                &Value::Sequence(Vec::new()),
            )?;
        }
        cs.write(&node, text);
        self.commit(cs).await
    }

    /// Add photographs to the album at `node`, each named by its digest
    /// (`sha256:<hex>`) or by its path in the source: relative to the
    /// manifest's root (`2019/lake.jpg`), or to the workspace
    /// (`photo/2019/lake.jpg`). New picks go at the end, in the order given.
    ///
    /// Refused, writing nothing, when one names nothing the source lists, or a
    /// row with no digest: a pick has to be something the album can ask for.
    pub async fn pick(&mut self, node: &Path, items: &[String]) -> Result<PickUpdate> {
        let node = link::normalize(node);
        let view = self.album(&node).await?.ok_or_else(|| not_album(&node))?;
        let mut album = view.album.clone();
        let mut update = PickUpdate::default();
        for item in items {
            let hash = find(&view.source, item)?;
            if album.picks(&hash) || update.changed.contains(&hash) {
                update.unchanged.push(hash);
            } else {
                album.picks.push(Pick::new(hash.clone()));
                update.changed.push(hash);
            }
        }
        update.total = album.picks.len();
        if !update.changed.is_empty() {
            self.write_picks(&node, &album).await?;
        }
        Ok(update)
    }

    /// Take photographs out of the album at `node`, named as for
    /// [`pick`](Self::pick). A digest the source no longer lists can still be
    /// taken out by its digest, which is how a missing pick is dropped.
    pub async fn unpick(&mut self, node: &Path, items: &[String]) -> Result<PickUpdate> {
        let node = link::normalize(node);
        let view = self.album(&node).await?.ok_or_else(|| not_album(&node))?;
        let mut album = view.album.clone();
        let mut update = PickUpdate::default();
        for item in items {
            let hash = if is_sha256(item) {
                item.clone()
            } else {
                find(&view.source, item)?
            };
            if album.picks(&hash) {
                album.picks.retain(|pick| pick.hash != hash);
                update.changed.push(hash);
            } else {
                update.unchanged.push(hash);
            }
        }
        update.total = album.picks.len();
        if !update.changed.is_empty() {
            self.write_picks(&node, &album).await?;
        }
        Ok(update)
    }

    async fn write_picks(&mut self, node: &Path, album: &Album) -> Result<()> {
        let (text, doc) = self.load(node).await?;
        let text = prov_store::edit::set_meta_in_text(
            &text,
            doc.carrier,
            PICKS_KEY,
            &album.picks_value(),
        )?;
        let mut cs = self.change();
        cs.write(node, text);
        self.commit(cs).await
    }
}

fn not_album(node: &Path) -> Error {
    Error::Structure(format!(
        "{} is not an album: it has no `{PICKS_FROM_KEY}`",
        node.display()
    ))
}

/// The digest of the source's row named by `item`: a digest it lists, or a
/// path relative to its root or to the workspace.
fn find(source: &AlbumSource, item: &str) -> Result<String> {
    if is_sha256(item) {
        return match source.row(item) {
            Some(_) => Ok(item.to_string()),
            None => Err(Error::Structure(format!(
                "{} does not list {item}",
                source.node.display()
            ))),
        };
    }
    let wanted = link::normalize(Path::new(item));
    let root = source.manifest.covered_root(&source.manifest_doc);
    let within: PathBuf = wanted
        .strip_prefix(&root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| wanted.clone());
    let row = source
        .manifest
        .files
        .iter()
        .find(|row| row.path == within || row.path == wanted)
        .ok_or_else(|| {
            Error::Structure(format!("{} lists no file at {item}", source.node.display()))
        })?;
    row.hash.clone().ok_or_else(|| {
        Error::Structure(format!(
            "{} lists {item} without a hash, so it cannot be picked",
            source.node.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::Finding;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_testkit::{read, write};

    const A: &str = "sha256:bb2cb0280300ae0e9a87c49ea385083afd40c33c8aa1abdc88faa321f4a27e76";
    const B: &str = "sha256:e6512e4c3757513df434c8d1e3565289c71dbe98a98fefae5c05795a0c5eda6b";
    const C: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn ws(dir: &Path) -> Workspace<StdFs> {
        Workspace::builder(StdFs).root(dir).build()
    }

    /// A library holding a held archive's node and manifest, whose files are
    /// not here, and an album page beside it.
    fn library(tag: &str) -> prov_testkit::Scratch {
        let dir = prov_testkit::scratch("album", tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Photographs\ncontents:\n- '[Photo](photo.yaml)'\n- '[Reunion](reunion.md)'\n---\n",
        );
        write(
            &dir,
            "photo.yaml",
            "title: Photo\npart_of: '[Photographs](index.md)'\nmanifest: photo.manifest.yaml\n",
        );
        write(
            &dir,
            "photo.manifest.yaml",
            format!(
                "root: photo/\nheld:\n  by: Harris NAS\nfiles:\n- path: 2019/lake.jpg\n  hash: {A}\n- path: 2020/snow.jpg\n  hash: {B}\n"
            ),
        );
        write(
            &dir,
            "reunion.md",
            "---\ntitle: Reunion\npart_of: '[Photographs](index.md)'\n---\n\nThe 2019 reunion.\n",
        );
        dir
    }

    fn album_findings(dir: &Path) -> Vec<Finding> {
        block_on(ws(dir).check(Path::new("index.md")))
            .unwrap()
            .into_iter()
            .filter(|f| {
                matches!(
                    f,
                    Finding::AlbumMalformed { .. }
                        | Finding::AlbumPickMissing { .. }
                        | Finding::BrokenLink { .. }
                )
            })
            .collect()
    }

    #[test]
    fn an_album_picks_by_path_or_digest_and_keeps_its_body() {
        let dir = library("picks");
        let mut w = ws(&dir);
        block_on(w.start_album(Path::new("reunion.md"), Path::new("photo.yaml"))).unwrap();
        let update = block_on(w.pick(
            Path::new("reunion.md"),
            &[
                "photo/2020/snow.jpg".into(),
                A.into(),
                "2020/snow.jpg".into(),
            ],
        ))
        .unwrap();
        assert_eq!(update.changed, [B, A]);
        assert_eq!(update.unchanged, [B]);
        assert_eq!(update.total, 2);

        let text = read(&dir, "reunion.md");
        assert!(text.contains("picks_from:"), "{text}");
        assert!(text.ends_with("The 2019 reunion.\n"), "{text}");

        let view = block_on(ws(&dir).album(Path::new("reunion.md")))
            .unwrap()
            .unwrap();
        assert_eq!(view.source.node, Path::new("photo.yaml"));
        let paths: Vec<_> = view
            .picked
            .iter()
            .map(|(_, row)| row.path.clone())
            .collect();
        assert_eq!(
            paths,
            [
                PathBuf::from("2020/snow.jpg"),
                PathBuf::from("2019/lake.jpg")
            ]
        );
        assert!(view.missing.is_empty());
        assert!(
            album_findings(&dir).is_empty(),
            "{:?}",
            album_findings(&dir)
        );
    }

    #[test]
    fn a_pick_must_be_something_the_source_lists() {
        let dir = library("refused");
        let mut w = ws(&dir);
        block_on(w.start_album(Path::new("reunion.md"), Path::new("photo.yaml"))).unwrap();
        let before = read(&dir, "reunion.md");
        for bad in [C, "2019/nowhere.jpg"] {
            assert!(
                block_on(w.pick(Path::new("reunion.md"), &[A.into(), bad.into()])).is_err(),
                "{bad}"
            );
        }
        assert_eq!(read(&dir, "reunion.md"), before, "nothing written");
    }

    #[test]
    fn a_pick_the_source_no_longer_lists_is_reported_and_can_be_dropped() {
        let dir = library("missing");
        write(
            &dir,
            "reunion.md",
            format!(
                "---\ntitle: Reunion\npart_of: '[Photographs](index.md)'\npicks_from: '[Photo](photo.yaml)'\npicks:\n- {A}\n- {C}\n---\n"
            ),
        );
        assert_eq!(
            album_findings(&dir),
            [Finding::AlbumPickMissing {
                album: "reunion.md".into(),
                source: "photo.yaml".into(),
                hash: C.into(),
            }]
        );
        let mut w = ws(&dir);
        let update = block_on(w.unpick(Path::new("reunion.md"), &[C.into()])).unwrap();
        assert_eq!(update.changed, [C]);
        assert_eq!(update.total, 1);
        assert!(album_findings(&dir).is_empty());
    }

    #[test]
    fn an_album_of_something_that_is_not_a_manifest_is_malformed() {
        let dir = library("not-manifest");
        write(
            &dir,
            "reunion.md",
            format!(
                "---\ntitle: Reunion\npart_of: '[Photographs](index.md)'\npicks_from: '[Photographs](index.md)'\npicks:\n- {A}\n---\n"
            ),
        );
        let found = album_findings(&dir);
        assert!(
            matches!(&found[..], [Finding::AlbumMalformed { doc, .. }] if doc == Path::new("reunion.md")),
            "{found:?}"
        );
        assert!(
            block_on(ws(&dir).start_album(Path::new("reunion.md"), Path::new("index.md"))).is_err()
        );
    }

    #[test]
    fn an_album_whose_source_is_gone_is_one_broken_link() {
        let dir = library("broken");
        write(
            &dir,
            "reunion.md",
            format!(
                "---\ntitle: Reunion\npart_of: '[Photographs](index.md)'\npicks_from: '[Gone](gone.yaml)'\npicks:\n- {A}\n---\n"
            ),
        );
        let found = album_findings(&dir);
        assert!(
            matches!(&found[..], [Finding::BrokenLink { doc, .. }] if doc == Path::new("reunion.md")),
            "{found:?}"
        );
    }

    #[test]
    fn renaming_the_source_rewrites_the_album() {
        let dir = library("rename");
        let mut w = ws(&dir);
        block_on(w.start_album(Path::new("reunion.md"), Path::new("photo.yaml"))).unwrap();
        block_on(w.pick(Path::new("reunion.md"), &[A.into()])).unwrap();
        let mut w = ws(&dir);
        block_on(w.rename(Path::new("photo.yaml"), Path::new("archive.yaml"))).unwrap();
        let text = read(&dir, "reunion.md");
        assert!(text.contains("archive.yaml"), "{text}");
        let view = block_on(ws(&dir).album(Path::new("reunion.md")))
            .unwrap()
            .unwrap();
        assert_eq!(view.source.node, Path::new("archive.yaml"));
        assert!(
            album_findings(&dir).is_empty(),
            "{:?}",
            album_findings(&dir)
        );
    }
}
