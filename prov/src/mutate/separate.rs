//! `separate` and `combine` — the two halves of a document's embedding shape.
//!
//! A combined document is one file (prose under a frontmatter block); a
//! separated one is two (a whole-file metadata node pointing at a body file
//! through `content`). Both verbs move the *structural* document from one path
//! to the other, so both retarget every inbound link and carry the registered
//! id across — the same maintenance [`rename`](super::rename) does, for a move
//! the caller never spells as one.

use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;
use prov_graph::document::MetaCarrier;
use prov_graph::error::{Error, Result};
use prov_graph::link;
use prov_graph::meta::Value;
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use super::maintain::content_target;

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Split the combined document at `path` into two linked plain-text files: a
    /// whole-file **metadata** document (in the document's own frontmatter
    /// format) that becomes the structural node, and a **body** file holding its
    /// prose, joined by a `content` attribute on the metadata file. Every inbound
    /// link to the document is retargeted to the new metadata file, and a
    /// registered ID follows it. Returns the metadata file's path. The inverse of
    /// [`combine`](Workspace::combine).
    pub async fn separate(&mut self, path: &Path) -> Result<PathBuf> {
        // Retargeting inbound links costs a census plus a load of every source
        // it turns up — see `rename`, which shares the collector.
        let _scope = self.read_scope();
        let path = link::normalize(path);
        if !self.exists(&path).await? {
            return Err(Error::NotFound(path.to_path_buf()));
        }
        let (own_text, doc) = self.load(&path).await?;
        let Some(MetaCarrier::Fenced(kind)) = doc.carrier else {
            return Err(Error::Structure(format!(
                "{} is not a combined document (nothing to separate)",
                path.display()
            )));
        };
        if doc.content_attr().is_some() {
            return Err(Error::Structure(format!(
                "{} is already separated",
                path.display()
            )));
        }
        let Some(mapping) = doc.meta.as_mapping() else {
            return Err(Error::Structure(format!(
                "{} has no metadata to separate",
                path.display()
            )));
        };
        let format = kind.inner_format();
        let meta_path = path.with_extension(prov_graph::document::whole_file_extension(format));
        if meta_path == path {
            return Err(Error::Structure(format!(
                "{} already has a metadata-file extension",
                path.display()
            )));
        }
        if self.exists(&meta_path).await? {
            return Err(Error::AlreadyExists(meta_path.to_path_buf()));
        }
        // `meta_path` is only an extension swap of `path` — derived, not freshly
        // minted, so it is not provably free of a registration nobody has a file
        // for (the on-disk check above cannot see one). Same guard as `rename`.
        let moving_id = self.index().id_for_path(&path);
        if let Some(id) = &moving_id
            && let Some(conflict) = self.move_conflict(id, &meta_path)
        {
            return Err(conflict.into());
        }
        let body_ref = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::Structure(format!("{} has no filename", path.display())))?
            .to_string();

        // The metadata file = the document's mapping + a `content` pointer at the
        // body file (a sibling, so just its name).
        let mut map = mapping.clone();
        map.insert("content".into(), Value::String(body_ref));
        let meta_text = prov_graph::meta::serialize_mapping(&map, format)?;
        let body_text = doc.body.clone();

        let mut cs = self.change();
        // The split was computed from this reading of the document, over a
        // census-wide window another writer can land in: stage the reading as
        // the set's expectation (a drifted document refuses the split rather
        // than shredding the racer's edit across two files), and the metadata
        // path's verified absence (so the refusal above cannot be raced into a
        // clobber).
        cs.expect(&path, own_text);
        cs.expect_absent(&meta_path);
        // Inbound links now point at the metadata file (the structural node).
        let inbound = self.collect_inbound_rewrites(&path, &meta_path).await?;

        cs.write(&meta_path, meta_text);
        cs.write(&path, body_text);
        for (source, rw) in inbound {
            cs.expect(&source, rw.read);
            cs.write(source, rw.text);
        }
        if let Some(id) = moving_id {
            self.index_mut().set_path(&id, &meta_path);
        }
        self.commit(cs).await?;
        Ok(meta_path)
    }

    /// Fold the separated document whose metadata file is `path` back into one
    /// combined file: the body file regains its metadata as frontmatter (in the
    /// metadata file's format), the metadata file is removed, and inbound links
    /// are retargeted to the combined file. Returns the combined file's path. The
    /// inverse of [`separate`](Workspace::separate).
    pub async fn combine(&mut self, path: &Path) -> Result<PathBuf> {
        // As in `separate`: retargeting the inbound links costs a census plus a
        // load of every source it turns up.
        let _scope = self.read_scope();
        let path = link::normalize(path);
        let (node_text, doc) = self.load(&path).await?;
        let Some(content) = content_target(&doc, &path) else {
            return Err(Error::Structure(format!(
                "{} is not a separated document (no `content` attribute)",
                path.display()
            )));
        };
        let Some(MetaCarrier::WholeFile(format)) = doc.carrier else {
            return Err(Error::Structure(format!(
                "{} is not a whole-file metadata document",
                path.display()
            )));
        };
        let Some(mapping) = doc.meta.as_mapping() else {
            return Err(Error::Structure(format!(
                "{} has no metadata",
                path.display()
            )));
        };
        if !self.exists(&content).await? {
            return Err(Error::Structure(format!(
                "{}'s content file {} is missing",
                path.display(),
                content.display()
            )));
        }
        // Unlike `meta_path` in `separate`, `content` already has a file behind
        // it — but that is no proof the *registry* agrees it is free: a stray
        // frontmatter (tolerated below) can carry its own `id`, distinct from
        // `path`'s. Same guard as `rename`/`separate`, before the merge is built.
        let moving_id = self.index().id_for_path(&path);
        if let Some(id) = &moving_id
            && let Some(conflict) = self.move_conflict(id, &content)
        {
            return Err(conflict.into());
        }
        let (body_raw, body_doc) = self.load(&content).await?;
        let body_read = body_raw.clone();
        // Normally the body file is pure prose; tolerate a stray frontmatter.
        let body = match body_doc.carrier {
            Some(_) => body_doc.body,
            None => body_raw,
        };

        // Rebuild the combined document: a fresh frontmatter block (the metadata
        // format) carrying every key except `content`, then the body.
        let carrier = prov_graph::document::frontmatter_carrier(format);
        let mut editor = MetaEditor::open_or_init(&body, Some(carrier))?;
        for (key, value) in mapping {
            if key.as_str() == "content" {
                continue;
            }
            editor.set_value(&[Segment::Key(key)], fig::Value::from(value))?;
        }
        let combined = editor.render()?;

        let mut cs = self.change();
        // The merge was computed from this reading of both halves — the node
        // that will be removed and the body it folds into. Stage both as
        // expectations, so a pair another writer edited in the compute→apply
        // gap refuses the fold rather than combining stale halves (and
        // removing the racer's version of the node outright).
        cs.expect(&path, node_text);
        cs.expect(&content, body_read);
        // Inbound links point back at the (now combined) content file.
        let inbound = self.collect_inbound_rewrites(&path, &content).await?;

        cs.write(&content, combined);
        cs.remove(&path);
        for (source, rw) in inbound {
            cs.expect(&source, rw.read);
            cs.write(source, rw.text);
        }
        if let Some(id) = moving_id {
            self.index_mut().set_path(&id, &content);
        }
        self.commit(cs).await?;
        Ok(content)
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use prov_graph::index::IdIndex;

    #[test]
    fn separate_refuses_to_take_a_path_the_registry_binds_to_a_different_id() {
        // `separate`'s metadata-file path is only an extension swap of the
        // combined document's own path — derived, not freshly minted, and no
        // more provably free of a live foreign registration than `rename`'s
        // destination is.
        let dir = tempdir("separate-path-collision");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- doc.md\n---\n",
        );
        write(
            &dir,
            "doc.md",
            "---\ntitle: Doc\npart_of: index.md\n---\nbody prose\n",
        );

        let mut w = id_ws(&dir);
        let a = prov_graph::identity::Id("aaaaaaa".into());
        let b = prov_graph::identity::Id("bbbbbbb".into());
        w.index_mut().register(&a, Path::new("doc.md"));
        // The metadata file `separate` would create is already bound to a
        // different id, though nothing has ever put a file there.
        w.index_mut().register(&b, Path::new("doc.yaml"));

        let err = block_on(w.separate(Path::new("doc.md"))).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Collision(prov_graph::index::Collision::Path { .. })
            ),
            "{err:?}"
        );
        assert!(err.to_string().contains("doc.yaml"), "{err}");

        assert!(dir.join("doc.md").exists());
        assert!(!dir.join("doc.yaml").exists());
        assert_eq!(w.index().resolve(&a), Some(PathBuf::from("doc.md")));
        assert_eq!(w.index().resolve(&b), Some(PathBuf::from("doc.yaml")));
    }

    #[test]
    fn combine_refuses_to_take_a_path_the_registry_binds_to_a_different_id() {
        // Unlike `separate`'s destination, `combine`'s (the body file) already
        // has a file behind it — but that is no proof the *registry* agrees it
        // is free: a stray frontmatter can carry its own `id`, distinct from the
        // node's. Guarded the same way, before the merge is built.
        let dir = tempdir("combine-path-collision");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- doc.yaml\n---\n",
        );
        write(
            &dir,
            "doc.yaml",
            "title: Doc\npart_of: index.md\ncontent: doc.md\n",
        );
        write(&dir, "doc.md", "the prose\n");

        let mut w = id_ws(&dir);
        let a = prov_graph::identity::Id("aaaaaaa".into());
        let b = prov_graph::identity::Id("bbbbbbb".into());
        w.index_mut().register(&a, Path::new("doc.yaml"));
        // The body file already carries a registration of its own, under a
        // different id than the node being folded into it.
        w.index_mut().register(&b, Path::new("doc.md"));

        let err = block_on(w.combine(Path::new("doc.yaml"))).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Collision(prov_graph::index::Collision::Path { .. })
            ),
            "{err:?}"
        );
        assert!(err.to_string().contains("doc.md"), "{err}");

        assert!(dir.join("doc.yaml").exists());
        assert_eq!(read(&dir, "doc.md"), "the prose\n", "unmerged, untouched");
        assert_eq!(w.index().resolve(&a), Some(PathBuf::from("doc.yaml")));
        assert_eq!(w.index().resolve(&b), Some(PathBuf::from("doc.md")));
    }
}
