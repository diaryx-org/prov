//! The recycle bin: `recycle`, `restore`, `empty_bin`.
//!
//! [`delete`](super::delete)'s recoverable counterpart, and the safe default for
//! archival use. The bin is a first-class, *reachable* member — an index
//! document the root links through the recycle relation, which `check` validates
//! like any other — recording per deletion where a document came from and where
//! its bytes now live, while the bytes themselves park under an *unreached*
//! `items/` so the orphan check (DESIGN §8) never mistakes a binned document for
//! a stray one.

use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::validate::Finding;
use crate::workspace::Workspace;
use prov_graph::document::Document;
use prov_graph::error::{Error, Result};
use prov_graph::graph::{LinkSite, Resolution, Target};
use prov_graph::link::{self, Link};
use prov_graph::meta::Value;
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use super::maintain::paired_file;

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Delete the document at `path` by moving it into the workspace **recycle
    /// bin** instead of destroying it — the recoverable counterpart of
    /// [`delete`](Self::delete), and the safe default for archival use.
    ///
    /// It shares `delete`'s structure — the parent's spanning entry is removed,
    /// a document with spanning children is refused unless `force`d, and the same
    /// dangling-inbound-reference diagnosis is returned — but rather than
    /// [`remove`](crate::ChangeSet::remove) the file it is **moved** into the bin
    /// and recorded there, so [`restore`](Self::restore) can bring it back.
    ///
    /// The bin is a first-class, reachable member: its index document (which the
    /// root links through the recycle relation, and which `check` validates like
    /// any other) records, per deletion, where the document came from and where
    /// its bytes now live. The whole operation — the file move, the parent edit,
    /// the bin-index update, and (the first time) the root's pointer to the bin —
    /// lands as one journaled [`ChangeSet`], so a bin-delete is exactly as
    /// crash-atomic as everything else.
    ///
    /// The deleted bytes are parked under `recyclebin/items/`, mirroring their
    /// original path. That subdirectory is deliberately *unreached* — nothing
    /// links into it — so the reachability-bounded orphan check (DESIGN §8) never
    /// mistakes a binned document for a stray one.
    ///
    /// `at` is an optional caller-supplied deletion timestamp recorded on the
    /// tombstone (the CLI passes the current time). The library takes it as an
    /// argument rather than reading a clock so the op stays deterministic.
    pub async fn recycle(
        &mut self,
        path: &Path,
        force: bool,
        at: Option<&str>,
    ) -> Result<Vec<Finding>> {
        let path = link::normalize(path);
        let (spanning, inverse) = self.spanning_pair()?;
        let (_, doc) = self.load(&path).await?;

        // Children guard — identical to `delete`'s: a document that contains
        // others would orphan them, so it is refused unless forced.
        let children: Vec<String> = self
            .relations()
            .children(&fig::Value::from(&doc.meta))
            .iter()
            .map(|raw| Link::parse(raw).target)
            .collect();
        if !children.is_empty() && !force {
            return Err(Error::Structure(format!(
                "{} contains {} document(s) ({}); delete them first or force",
                path.display(),
                children.len(),
                children.join(", ")
            )));
        }

        let parent = self.single_target(&doc, &inverse, &path);
        let root = self.spanning_root(&path, &inverse).await?;

        // Separated-body guard — identical to `delete`'s, and it matters more
        // here: the CLI's `rm` routes to the bin by default, so this is the path
        // a user actually reaches. Binning a node's prose half leaves the node
        // pointing at nothing, and the census below cannot see `content`.
        let owner = self.content_owner(&path).await?;
        if let Some(owner) = &owner
            && !force
        {
            return Err(Error::Structure(format!(
                "{} is the body of {}; recycle that instead, or force to bin the \
                 body and leave {} pointing at nothing",
                path.display(),
                owner.display(),
                owner.display()
            )));
        }

        // Inbound references the move leaves dangling — the same diagnosis
        // `delete` returns, since a binned document is out of the live graph.
        let mut danglers: Vec<Finding> = self
            .census(&root)
            .await?
            .into_iter()
            .filter(|e| e.resolution.resolved_path() == Some(&path))
            .filter(|e| {
                e.source != path
                    && !(Some(&e.source) == parent.as_ref()
                        && matches!(&e.site, LinkSite::Relation(r) if *r == spanning))
            })
            .map(|e| match e.resolution {
                Resolution::Id { id, .. } => Finding::DanglingId {
                    doc: e.source,
                    site: e.site,
                    id,
                    tombstoned: true,
                },
                _ => Finding::BrokenLink {
                    doc: e.source,
                    site: e.site,
                    target: e.target_text,
                },
            })
            .collect();

        // A forced body recycle strands its node's `content` pointer, and the
        // census cannot report it (`content` is neither a relation nor a body
        // link). Reported here so the verb's diagnosis covers the separated
        // shape too — see `delete`, which does the same.
        if let Some(owner) = owner {
            let target = self
                .load(&owner)
                .await
                .ok()
                .and_then(|(_, doc)| doc.content_attr().map(str::to_string))
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            danglers.push(Finding::BrokenLink {
                doc: owner,
                site: LinkSite::Relation("content".to_string()),
                target,
            });
        }

        // Locate the bin, or plan to bootstrap it on this first deletion.
        let format = self.default_embed_format();
        let ext = prov_graph::document::whole_file_extension(format);
        let existing_index = self.recycle_bin_path(&root).await?;
        let bin_index = existing_index
            .clone()
            .unwrap_or_else(|| PathBuf::from("recyclebin").join(format!("index.{ext}")));
        let bin_dir = bin_index
            .parent()
            .unwrap_or(Path::new("recyclebin"))
            .to_path_buf();
        let items_dir = bin_dir.join("items");

        // The bin index's current records (and its own title, so a wholesale
        // re-render preserves it). The bin is machinery, reached one-way through
        // the root's `recycle_bin` pointer, so it carries no `part_of` back-link
        // (DESIGN §5, "link target kinds"). Absent bin → empty, with a default title.
        let (mut records, bin_title) = match &existing_index {
            Some(index) => {
                let (_, bin_doc) = self.load(index).await?;
                // The bin index is a record store — reject a markdown carrier
                // (DESIGN §5, whole-file rule).
                if let Some(carrier) = bin_doc.carrier {
                    prov_graph::document::require_whole_file(index, carrier)?;
                }
                let recs = bin_doc
                    .meta
                    .get("deleted")
                    .and_then(Value::as_sequence)
                    .map(<[Value]>::to_vec)
                    .unwrap_or_default();
                let title = bin_doc
                    .meta
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Recycle Bin")
                    .to_string();
                (recs, title)
            }
            None => (Vec::new(), "Recycle Bin".to_string()),
        };

        // Where the bytes go: mirror the original path under the (unreached)
        // items directory, with a numeric suffix on the rare same-path collision.
        let mut node_bin = items_dir.join(&path);
        let mut bump = 1;
        while self.exists(&node_bin).await? {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            node_bin = items_dir
                .join(path.parent().unwrap_or(Path::new("")))
                .join(format!("{name}.{bump}"));
            bump += 1;
        }

        // A separated document's prose body — or a manifest node's record store
        // — travels with it into the bin, so `restore` brings back the pair. The
        // directory a manifest covers stays where it is: recycling a description
        // is not recycling the archive it describes.
        let body_from = paired_file(&doc, &path);
        let body_bin = match &body_from {
            Some(body) if self.exists(body).await? => Some((body.clone(), items_dir.join(body))),
            _ => None,
        };

        // The tombstone record — everything `restore` needs to undo this.
        let title = doc
            .meta
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| link::path_to_title(&path));
        let id_opt = self.index().id_for_path(&path);
        let mut record = prov_graph::meta::Mapping::new();
        record.insert("title".into(), Value::String(title));
        if let Some(id) = &id_opt {
            record.insert("id".into(), Value::String(id.to_string()));
        }
        record.insert(
            "from".into(),
            Value::String(path.to_string_lossy().into_owned()),
        );
        record.insert(
            "bin".into(),
            Value::String(node_bin.to_string_lossy().into_owned()),
        );
        if let Some(parent) = &parent {
            record.insert(
                "parent".into(),
                Value::String(parent.to_string_lossy().into_owned()),
            );
        }
        if let Some((from, to)) = &body_bin {
            record.insert(
                "body_from".into(),
                Value::String(from.to_string_lossy().into_owned()),
            );
            record.insert(
                "body_bin".into(),
                Value::String(to.to_string_lossy().into_owned()),
            );
        }
        if let Some(at) = at {
            record.insert("at".into(), Value::String(at.to_string()));
        }
        records.push(Value::Mapping(record));

        let mut bin_map = prov_graph::meta::Mapping::new();
        bin_map.insert("title".into(), Value::String(bin_title));
        bin_map.insert("deleted".into(), Value::Sequence(records));
        let bin_text = prov_graph::meta::serialize_mapping(&bin_map, format)?;

        // The parent's spanning entry for the doomed document, removed.
        let mut parent_write: Option<(PathBuf, String)> = None;
        if let Some(parent) = &parent {
            let (parent_text, parent_doc) = self.load(parent).await?;
            if let (Some(index), Some(carrier)) = (
                self.entry_index(&parent_doc, &spanning, parent, &path),
                parent_doc.carrier,
            ) {
                let mut editor = MetaEditor::open(&parent_text, carrier)?;
                editor.remove_item(&[Segment::Key(&spanning)], index)?;
                parent_write = Some((parent.clone(), editor.render()?));
            }
        }

        let mut cs = self.change();
        cs.rename(&path, &node_bin);
        if let Some((from, to)) = &body_bin {
            cs.rename(from, to);
        }
        cs.write(&bin_index, bin_text);

        // The root's pointer to the bin, authored the first time only — merged
        // with the parent edit when the parent *is* the root, so the one document
        // is written once with both changes rather than twice.
        let mut root_text: Option<String> = None;
        if let Some((parent_path, text)) = &parent_write {
            if *parent_path == root {
                root_text = Some(text.clone());
            } else {
                cs.write(parent_path.clone(), text.clone());
            }
        }
        // Link the bin from the root, the first time one is created — but never
        // into the document being recycled.
        //
        // `root` here is `spanning_root` walked up from the subject, and for a
        // subject with no spanning parent (an orphan — which `check` reports and
        // users routinely bin — or a separated body under `force`) that walk
        // lands back on the subject itself. Writing the pointer there staged a
        // write to a path the same change set had just renamed away, which
        // recreated the file in place: the user saw the document still sitting
        // there, now carrying a `recycle_bin:` key it never had, *and* a copy in
        // the bin.
        //
        // There is no reachable root to link from in that case, so the bin is
        // simply left unlinked and the next recycle from a real node adopts it
        // (`existing_index` is discovered by path, not by the pointer). A bin
        // nothing links to is a finding `check` can raise; a deleted document
        // that reappears with machinery stamped into it is data loss wearing a
        // success message.
        if existing_index.is_none() && root != path {
            let base = match &root_text {
                Some(text) => text.clone(),
                None => self.load(&root).await?.0,
            };
            let root_doc = Document::parse(&root, &base)?;
            let relation = self
                .relations()
                .recycle_relation()
                .ok_or_else(|| Error::Structure("no recycle relation configured".into()))?
                .to_string();
            let style = self.reference_style_for(&relation).path_style;
            let pointer = link::path_text(style, &root, &bin_index);
            root_text = Some(prov_store::edit::set_in_text(
                &base,
                root_doc.carrier,
                &relation,
                prov_store::edit::infer_scalar(&pointer),
            )?);
        }
        if let Some(text) = root_text {
            cs.write(root.clone(), text);
        }

        // Identity hook — retire the ID to a tombstone exactly as `delete` does;
        // the record keeps its value so `restore` can re-register it.
        if let Some(id) = &id_opt {
            self.index_mut().unregister(id);
        }
        self.commit(cs).await?;
        Ok(danglers)
    }

    /// Bring a document back from the recycle bin to the path it was deleted
    /// from — the inverse of [`recycle`](Self::recycle).
    ///
    /// The tombstone record carries everything needed: the bin location to move
    /// the bytes back from, the parent to re-link under (only the parent → child
    /// direction was lost; the child's own inverse link travelled with it, so it
    /// is correct again the moment the file is home), and the ID to re-register.
    /// It all lands as one journaled [`ChangeSet`]. Refuses when something already
    /// occupies the restore path, or when `from` is not in the bin.
    ///
    /// `root_doc` names the workspace root, from which the bin is discovered.
    pub async fn restore(&mut self, from: &Path, root_doc: &Path) -> Result<()> {
        let from = link::normalize(from);
        let (spanning, _) = self.spanning_pair()?;
        let bin_index = self
            .recycle_bin_path(root_doc)
            .await?
            .ok_or_else(|| Error::Structure("workspace has no recycle bin".into()))?;
        let (_, bin_doc) = self.load(&bin_index).await?;
        // The bin index is a record store — reject a markdown carrier
        // (DESIGN §5, whole-file rule).
        if let Some(carrier) = bin_doc.carrier {
            prov_graph::document::require_whole_file(&bin_index, carrier)?;
        }
        let records: Vec<Value> = bin_doc
            .meta
            .get("deleted")
            .and_then(Value::as_sequence)
            .map(<[Value]>::to_vec)
            .unwrap_or_default();

        let from_str = from.to_string_lossy();
        let pos = records
            .iter()
            .position(|r| r.get("from").and_then(Value::as_str) == Some(from_str.as_ref()))
            .ok_or_else(|| {
                Error::Structure(format!("{} is not in the recycle bin", from.display()))
            })?;
        let record = records[pos].clone();
        let str_field = |key: &str| record.get(key).and_then(Value::as_str).map(str::to_owned);
        let node_bin = PathBuf::from(
            str_field("bin")
                .ok_or_else(|| Error::Structure("recycle record has no `bin` path".into()))?,
        );
        let parent = str_field("parent").map(PathBuf::from);
        let id = str_field("id").map(prov_graph::identity::Id);
        let title = str_field("title").unwrap_or_else(|| link::path_to_title(&from));
        let body = match (str_field("body_from"), str_field("body_bin")) {
            (Some(from), Some(bin)) => Some((PathBuf::from(from), PathBuf::from(bin))),
            _ => None,
        };

        // The record's id has been out of the registry since the delete, and the
        // workspace has moved on meanwhile — a sync can land a document that
        // spells it, or leave another id registered at the path this is coming
        // back to. Re-registering across either would take an id from a document
        // whose own frontmatter still claims it, so the restore refuses instead:
        // only the author can say which document should keep it. Checked up
        // front, beside the path guard, before any byte moves.
        if let Some(id) = &id
            && let Some(conflict) = self.registration_conflict(id, &from)
        {
            return Err(conflict.into());
        }

        if self.exists(&from).await? {
            return Err(Error::Structure(format!(
                "{} already exists; cannot restore over it",
                from.display()
            )));
        }

        // The bin index without this record, re-rendered whole (a machine file).
        let mut remaining = records;
        remaining.remove(pos);
        let format = self.default_embed_format();
        let part_of_style = self.reference_style_for("part_of").path_style;
        let mut bin_map = prov_graph::meta::Mapping::new();
        bin_map.insert(
            "title".into(),
            Value::String(
                bin_doc
                    .meta
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Recycle Bin")
                    .to_string(),
            ),
        );
        bin_map.insert(
            "part_of".into(),
            Value::String(
                bin_doc
                    .meta
                    .get("part_of")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| link::path_text(part_of_style, &bin_index, root_doc)),
            ),
        );
        bin_map.insert("deleted".into(), Value::Sequence(remaining));
        let bin_text = prov_graph::meta::serialize_mapping(&bin_map, format)?;

        let mut cs = self.change();
        // Re-register the ID *after* `change`'s checkpoint, so authoring the
        // parent link below reuses the document's own id rather than minting a new
        // one, and so a failure rolls the re-registration back with everything else.
        if let Some(id) = &id {
            self.index_mut().register(id, &from);
        }
        cs.rename(&node_bin, &from);
        if let Some((body_from, body_bin)) = &body {
            cs.rename(body_bin, body_from);
        }
        cs.write(&bin_index, bin_text);

        // Re-add the parent's spanning entry (its removal is all `recycle` did to
        // the parent). Skip when the parent is gone or already links the child.
        if let Some(parent) = &parent
            && self.exists(parent).await?
        {
            let (parent_text, parent_doc) = self.load(parent).await?;
            let already = self
                .relations()
                .children(&fig::Value::from(&parent_doc.meta))
                .iter()
                .any(|t| self.resolve_link(parent, &Link::parse(t)) == Target::Path(from.clone()));
            if !already {
                let down = self
                    .authored_target(&spanning, parent, &from, &title, false)
                    .await?;
                let mut editor = MetaEditor::open_or_init(&parent_text, parent_doc.carrier)?;
                let span_path = [Segment::Key(&spanning)];
                if editor
                    .append_value(&span_path, fig::Value::Str(down.clone()))
                    .is_err()
                {
                    editor.set_value(&span_path, fig::Value::Seq(vec![fig::Value::Str(down)]))?;
                }
                cs.write(parent.clone(), editor.render()?);
            }
        }
        self.commit(cs).await
    }

    /// Permanently purge every document in the recycle bin — the only hard
    /// delete, and always explicit. Returns how many records were purged.
    ///
    /// The bin's bytes are removed and its index emptied (the index member itself
    /// stays, still linked from the root), as one journaled [`ChangeSet`]. ID
    /// tombstones are untouched: an ID retired at deletion stays retired, so a
    /// `colophon:<id>` reference to a purged document remains diagnosable rather
    /// than silently reissuable.
    pub async fn empty_bin(&mut self, root_doc: &Path) -> Result<usize> {
        let bin_index = self
            .recycle_bin_path(root_doc)
            .await?
            .ok_or_else(|| Error::Structure("workspace has no recycle bin".into()))?;
        let (_, bin_doc) = self.load(&bin_index).await?;
        // The bin index is a record store — reject a markdown carrier
        // (DESIGN §5, whole-file rule).
        if let Some(carrier) = bin_doc.carrier {
            prov_graph::document::require_whole_file(&bin_index, carrier)?;
        }
        let records: Vec<Value> = bin_doc
            .meta
            .get("deleted")
            .and_then(Value::as_sequence)
            .map(<[Value]>::to_vec)
            .unwrap_or_default();
        let count = records.len();

        let format = self.default_embed_format();
        let part_of_style = self.reference_style_for("part_of").path_style;
        let mut bin_map = prov_graph::meta::Mapping::new();
        bin_map.insert(
            "title".into(),
            Value::String(
                bin_doc
                    .meta
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Recycle Bin")
                    .to_string(),
            ),
        );
        bin_map.insert(
            "part_of".into(),
            Value::String(
                bin_doc
                    .meta
                    .get("part_of")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| link::path_text(part_of_style, &bin_index, root_doc)),
            ),
        );
        bin_map.insert("deleted".into(), Value::Sequence(Vec::new()));
        let bin_text = prov_graph::meta::serialize_mapping(&bin_map, format)?;

        let mut cs = self.change();
        for record in &records {
            for key in ["bin", "body_bin"] {
                if let Some(path) = record.get(key).and_then(Value::as_str) {
                    let rel = PathBuf::from(path);
                    if self.exists(&rel).await? {
                        cs.remove(rel);
                    }
                }
            }
        }
        cs.write(&bin_index, bin_text);
        self.commit(cs).await?;
        Ok(count)
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;

    #[test]
    fn recycling_a_parentless_document_does_not_resurrect_it() {
        // Found while fixing the separated-body guard, and older than it: the
        // bin-bootstrap wrote its pointer into `spanning_root(subject)`, which
        // for a document with no parent *is the subject*. The change set renamed
        // the file into the bin and then wrote it straight back, so `prov rm` on
        // an orphan left the document in place — now carrying a `recycle_bin:`
        // key — beside a copy in the bin.
        //
        // Orphans are precisely what a user bins: `check` reports them as the
        // onboarding signal, and the answer is often "this was junk".
        let dir = tempdir("recycle-parentless");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(&dir, "loose.md", "---\ntitle: Loose\n---\nno parent\n");

        block_on(ws(&dir).recycle(Path::new("loose.md"), false, None)).unwrap();

        assert!(!dir.join("loose.md").exists(), "gone, and it stays gone");
        assert_eq!(
            read(&dir, "recyclebin/items/loose.md"),
            "---\ntitle: Loose\n---\nno parent\n"
        );
    }

    #[test]
    fn recycle_refuses_a_separated_body_and_names_its_node() {
        // `delete`'s guard, on the path a user actually takes: the CLI routes
        // `rm` to the bin unless `--purge`, so binning a node's prose half is
        // the reachable way to strand a `content` pointer.
        let dir = tempdir("recycle-separated-body");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- b.yaml\n---\n",
        );
        write(
            &dir,
            "b.yaml",
            "title: B\npart_of: index.md\ncontent: b.md\n",
        );
        write(&dir, "b.md", "B body.\n");

        let err = block_on(ws(&dir).recycle(Path::new("b.md"), false, None)).unwrap_err();
        assert!(err.to_string().contains("is the body of b.yaml"), "{err}");
        assert!(dir.join("b.md").exists(), "nothing was moved");

        // Forced, it bins the body and reports the pointer it stranded.
        let danglers = block_on(ws(&dir).recycle(Path::new("b.md"), true, None)).unwrap();
        assert!(!dir.join("b.md").exists());
        assert!(
            danglers.iter().any(|f| matches!(f,
                Finding::BrokenLink { doc, site: LinkSite::Relation(r), target }
                    if doc == &PathBuf::from("b.yaml") && r == "content" && target == "b.md")),
            "{danglers:?}"
        );
    }

    #[test]
    fn recycle_moves_a_document_into_the_bin_and_records_it() {
        let dir = tempdir("recycle-basic");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        let original = "---\ntitle: My Note\npart_of: index.md\n---\nbody text\n";
        write(&dir, "note.md", original);

        let danglers =
            block_on(ws(&dir).recycle(Path::new("note.md"), false, Some("2026-07-16T10:00:00Z")))
                .unwrap();
        assert!(danglers.is_empty(), "{danglers:?}");

        // The document is gone from its path but not destroyed — its bytes are
        // moved verbatim into the bin, under the (unreached) items directory.
        assert!(!dir.join("note.md").exists());
        assert_eq!(read(&dir, "recyclebin/items/note.md"), original);

        // The parent no longer links it, and the root now links the bin.
        let index = read(&dir, "index.md");
        assert!(
            !index.contains("- note.md"),
            "parent entry removed: {index}"
        );
        assert!(index.contains("recycle_bin"), "root links the bin: {index}");

        // The bin index records the deletion: title, origin, and timestamp.
        let bin = read(&dir, "recyclebin/index.yaml");
        assert!(bin.contains("My Note"), "records the title: {bin}");
        assert!(bin.contains("note.md"), "records the origin: {bin}");
        assert!(bin.contains("2026-07-16T10:00:00Z"), "records when: {bin}");

        // And the workspace is still consistent — the binned doc is *not* an orphan.
        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(
            findings.is_empty(),
            "a recycle should leave check clean: {findings:?}"
        );
    }

    #[test]
    fn recycle_then_restore_is_lossless() {
        // The round-trip is the whole promise: delete and restore return the
        // workspace to byte-identical state, parent link and all.
        let dir = tempdir("recycle-restore");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        let original = "---\ntitle: My Note\npart_of: index.md\n---\nbody text\n";
        write(&dir, "note.md", original);

        block_on(ws(&dir).recycle(Path::new("note.md"), false, None)).unwrap();
        assert!(!dir.join("note.md").exists());

        block_on(ws(&dir).restore(Path::new("note.md"), Path::new("index.md"))).unwrap();

        // The document is back, byte-for-byte.
        assert_eq!(read(&dir, "note.md"), original);
        // The parent links it again, and its record is gone from the bin.
        let index = read(&dir, "index.md");
        assert!(
            index.contains("note.md"),
            "parent re-links the restored doc: {index}"
        );
        let bin = read(&dir, "recyclebin/index.yaml");
        assert!(
            !bin.contains("My Note"),
            "the record is cleared on restore: {bin}"
        );
        assert!(
            !dir.join("recyclebin/items/note.md").exists(),
            "the binned bytes moved back"
        );
        // Consistent.
        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(
            findings.is_empty(),
            "a restore should leave check clean: {findings:?}"
        );
    }

    #[test]
    fn a_restore_refuses_to_take_an_id_from_the_document_that_now_holds_it() {
        // The bin record carries the id, and that id has been out of the registry
        // since the delete. `id_storage` defaults to `both`, so a sync can land a
        // document that spells it meanwhile — and re-registering would take the id
        // from a document whose own frontmatter still claims it, leaving the
        // registry naming one of two files that both say they are it. Only the
        // author can settle that, so the restore refuses.
        let dir = tempdir("restore-id-collision");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n- other.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: My Note\npart_of: index.md\nid: b7k2m\n---\nbody\n",
        );
        write(
            &dir,
            "other.md",
            "---\ntitle: Other\npart_of: index.md\nid: b7k2m\n---\n",
        );

        let mut w = id_ws(&dir);
        let id = prov_graph::identity::Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("note.md"));
        block_on(w.recycle(Path::new("note.md"), false, None)).unwrap();
        // The arrival: while note.md sat in the bin, the id turned up elsewhere.
        w.index_mut().register(&id, Path::new("other.md"));

        let err = block_on(w.restore(Path::new("note.md"), Path::new("index.md"))).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Collision(prov_graph::index::Collision::Id { .. })
            ),
            "{err:?}"
        );
        assert!(
            err.to_string().contains("other.md"),
            "the message must name what holds the id: {err}"
        );
        // Refused up front: not a byte moved.
        assert!(!dir.join("note.md").exists());
        assert!(dir.join("recyclebin/items/note.md").exists());

        // Precise, not blanket: once nothing else claims the id, it restores.
        w.index_mut().unregister(&id);
        block_on(w.restore(Path::new("note.md"), Path::new("index.md"))).unwrap();
        assert!(dir.join("note.md").exists());
    }

    #[test]
    fn a_restore_refuses_when_another_id_already_claims_the_path() {
        // The other direction, which the existing "already exists" guard cannot
        // see: the file is gone (so the path looks free) but the registry still
        // binds that path to a different id. Restoring would drop that id out of
        // the registry while the document still spells it — a live id silently
        // demoted to an unregistered one.
        let dir = tempdir("restore-path-collision");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: My Note\npart_of: index.md\nid: b7k2m\n---\nbody\n",
        );

        let mut w = id_ws(&dir);
        w.index_mut().register(
            &prov_graph::identity::Id("b7k2m".into()),
            Path::new("note.md"),
        );
        block_on(w.recycle(Path::new("note.md"), false, None)).unwrap();
        w.index_mut().register(
            &prov_graph::identity::Id("zzzzzzz".into()),
            Path::new("note.md"),
        );

        let err = block_on(w.restore(Path::new("note.md"), Path::new("index.md"))).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Collision(prov_graph::index::Collision::Path { .. })
            ),
            "{err:?}"
        );
        assert!(!dir.join("note.md").exists(), "nothing moved");
    }

    #[test]
    fn recycle_refuses_a_parent_with_children_unless_forced() {
        // Parity with `delete`: a document that contains others cannot be binned
        // without `force`, since binning it would strand them.
        let dir = tempdir("recycle-children");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(
            &dir,
            "a.md",
            "---\ntitle: A\npart_of: index.md\ncontents:\n- b.md\n---\n",
        );
        write(&dir, "b.md", "---\ntitle: B\npart_of: a.md\n---\n");

        let err = block_on(ws(&dir).recycle(Path::new("a.md"), false, None)).unwrap_err();
        assert!(err.to_string().contains("contains 1 document"), "{err}");
        assert!(
            dir.join("a.md").exists(),
            "a refused recycle changes nothing"
        );

        block_on(ws(&dir).recycle(Path::new("a.md"), true, None)).unwrap();
        assert!(!dir.join("a.md").exists());
        assert!(dir.join("recyclebin/items/a.md").exists());
    }

    #[test]
    fn a_second_deletion_appends_to_the_existing_bin() {
        // The bin is bootstrapped once; a later deletion appends to it, and the
        // root's pointer is authored a single time.
        let dir = tempdir("recycle-append");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n- b.md\n---\n");
        write(&dir, "a.md", "---\ntitle: Aye\npart_of: index.md\n---\n");
        write(&dir, "b.md", "---\ntitle: Bee\npart_of: index.md\n---\n");

        block_on(ws(&dir).recycle(Path::new("a.md"), false, None)).unwrap();
        block_on(ws(&dir).recycle(Path::new("b.md"), false, None)).unwrap();

        let bin = read(&dir, "recyclebin/index.yaml");
        assert!(
            bin.contains("Aye") && bin.contains("Bee"),
            "both recorded: {bin}"
        );
        assert!(dir.join("recyclebin/items/a.md").exists());
        assert!(dir.join("recyclebin/items/b.md").exists());

        let index = read(&dir, "index.md");
        assert_eq!(
            index.matches("recycle_bin").count(),
            1,
            "pointer authored once: {index}"
        );

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn empty_bin_purges_the_bytes_and_clears_the_index_but_keeps_the_member() {
        let dir = tempdir("recycle-empty");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\ntitle: Aye\npart_of: index.md\n---\n");

        block_on(ws(&dir).recycle(Path::new("a.md"), false, None)).unwrap();
        assert!(dir.join("recyclebin/items/a.md").exists());

        let purged = block_on(ws(&dir).empty_bin(Path::new("index.md"))).unwrap();
        assert_eq!(purged, 1);
        assert!(!dir.join("recyclebin/items/a.md").exists(), "bytes purged");

        let bin = read(&dir, "recyclebin/index.yaml");
        assert!(!bin.contains("Aye"), "records cleared: {bin}");
        // The bin member itself survives, still linked and consistent.
        assert!(read(&dir, "index.md").contains("recycle_bin"));
        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn a_failed_recycle_leaves_the_workspace_untouched() {
        // The whole move is one journaled ChangeSet, so an I/O failure part-way
        // rolls back to exactly the starting state — nothing half-binned.
        let dir = tempdir("recycle-atomic");
        write(&dir, "index.md", "---\ncontents:\n- note.md\n---\n");
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\nbody\n",
        );
        let before = snapshot(&dir);

        let mut w = Workspace::builder(FailAtWrite::nth(0)).root(&dir).build();
        let err = block_on(w.recycle(Path::new("note.md"), false, None)).unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");

        assert_eq!(
            snapshot(&dir),
            before,
            "a failed recycle tore the workspace"
        );
    }
}
