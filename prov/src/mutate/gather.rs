//! `gather` and `scatter` — attachments into a manifest, and a manifest back
//! into attachments.
//!
//! Twenty photographs added one at a time are twenty attachment sidecars, each
//! a record with a title, an id and a place in its parent's list. When they
//! turn out to be one thing — an afternoon, an album — the record that fits
//! is a [manifest](crate::manifest): one node, one list, the files in a
//! directory of their own. [`gather`](Workspace::gather) is that change, and
//! [`scatter`](Workspace::scatter) the reverse, for the album that turns out
//! to be twenty things after all.
//!
//! ## Nothing is lost on the way
//!
//! The two shapes do not hold the same things. A sidecar is a whole record
//! and a document; a manifest row is a path, a digest and fields of its own,
//! which the graph does not read. A field every gathered sidecar carries with
//! the *same* value moves onto the node, since it was a fact about all of
//! them, and any other — a date set on one photograph, a caption, a title
//! someone wrote rather than the one the file name gives — onto that
//! sidecar's row; a scatter puts a row's fields back on its sidecar. So each
//! verb first works out what the other shape cannot carry
//! ([`plan_gather`](Workspace::plan_gather),
//! [`plan_scatter`](Workspace::plan_scatter)) and refuses while any of it is
//! left:
//!
//! - **A field** the destination has no room for: on the way in, one that
//!   holds a link (a row is not read for links, so it would be one no rename
//!   rewrites and no `check` reports broken) or that a row keeps for itself
//!   (`path`, `hash`); on the way out, a row field a sidecar keeps for its
//!   own bookkeeping (`content`, `id`, …), or the node's title when the node
//!   goes. A field is the one loss a caller may accept, by naming it in
//!   [`RegroupOptions::discard`] — the person said so.
//! - **A link** to a record that will stop existing, or to a payload that
//!   will move — a transcript naming the photograph, a page embedding it.
//!   Never accepted: the link would break, and rewriting it to point at a
//!   whole album would change what it says.
//! - **Children.** A sidecar with records of its own under it has nowhere to
//!   put them in a manifest row. Never accepted.
//!
//! A record's own bookkeeping is not a loss: its `id` (the record is
//! replaced, and nothing may link to it), its link up to the parent, its
//! `content` pointer and `attachment` flag, its `content_hash` (it becomes the
//! row's digest, or the row's digest becomes it), the workspace's `updated`
//! stamp, and a title that is only the file name read as one.
//!
//! ## One change set
//!
//! Each verb lands as one change set: files moved, sidecars written or
//! removed, the manifest and node written or removed, the parent's list
//! rewritten — so an interruption leaves the workspace before or after, never
//! a directory half covered.

use std::path::{Path, PathBuf};

use fig::Segment;

use crate::attach::sidecar_path;
use crate::identity::{IdentityPolicy, Trigger};
use crate::workspace::Workspace;
use prov_graph::error::{Error, Result};
use prov_graph::graph::LinkSite;
use prov_graph::link;
use prov_graph::manifest::{
    HASH_KEY, MANIFEST_KEY, Manifest, ManifestEntry, PATH_KEY, manifest_sibling,
};
use prov_graph::meta::{Mapping, Value};
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

/// What one record would lose in a [`gather`](Workspace::gather) or a
/// [`scatter`](Workspace::scatter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loss {
    /// The record that would lose it — a sidecar being gathered, or the node
    /// being scattered.
    pub record: PathBuf,
    /// What.
    pub what: LossKind,
}

/// The kinds of thing a regrouping cannot carry. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LossKind {
    /// A metadata field the other shape has no room for.
    Field(String),
    /// A link from `from` to `to` — the record, or its payload — that the
    /// regrouping would break.
    Linked {
        /// The document holding the link.
        from: PathBuf,
        /// What it links to.
        to: PathBuf,
    },
    /// Records contained by this one, which have nowhere to go.
    Children(usize),
}

impl LossKind {
    /// Whether a caller may accept this loss — only a field, by name.
    pub fn discardable(&self) -> bool {
        matches!(self, LossKind::Field(_))
    }
}

/// What a caller agrees to when regrouping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegroupOptions {
    /// Fields whose loss is accepted, by name — `created`, `date_of_document`.
    /// A field named here is dropped, too, where a gather would otherwise
    /// have kept it on a row. Links and children are never accepted.
    pub discard: Vec<String>,
}

/// One payload a [`gather`](Workspace::gather) moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatheredFile {
    /// The sidecar it is being taken from.
    pub card: PathBuf,
    /// Where the payload is.
    pub from: PathBuf,
    /// Where it goes, under the new directory.
    pub to: PathBuf,
}

/// What [`plan_gather`](Workspace::plan_gather) found — and, from
/// [`gather`](Workspace::gather), what was done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatherPlan {
    /// The index the sidecars are in, which the node goes under.
    pub parent: PathBuf,
    /// The manifest node to be written.
    pub node: PathBuf,
    /// The manifest document to be written.
    pub manifest: PathBuf,
    /// The directory the payloads move into.
    pub root: PathBuf,
    /// Each payload's move, in the order the sidecars were given.
    pub files: Vec<GatheredFile>,
    /// Fields every sidecar carries with one value, moved onto the node.
    pub carried: Vec<String>,
    /// Fields only some sidecars carry, or carry differently, kept on the
    /// rows of the ones that do.
    pub on_rows: Vec<String>,
    /// What would be lost. A gather refuses while any is not accepted.
    pub losses: Vec<Loss>,
}

/// What [`plan_scatter`](Workspace::plan_scatter) found — and, from
/// [`scatter`](Workspace::scatter), what was done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScatterPlan {
    /// The manifest node being scattered.
    pub node: PathBuf,
    /// Its manifest document, removed.
    pub manifest: PathBuf,
    /// The index the new sidecars go under: the node itself, kept as an
    /// index, or the one the caller named.
    pub into: PathBuf,
    /// Whether the node is kept (as the index) or removed.
    pub keeps_node: bool,
    /// The sidecar to be written for each covered file, in the manifest's
    /// order, with its payload.
    pub cards: Vec<(PathBuf, PathBuf)>,
    /// Fields of the node copied onto every sidecar, when the node is removed.
    /// A row's own field of the same name wins on that row's sidecar.
    pub carried: Vec<String>,
    /// Fields the rows carry, each put back on its own sidecar.
    pub from_rows: Vec<String>,
    /// What would be lost. A scatter refuses while any is not accepted.
    pub losses: Vec<Loss>,
}

impl GatherPlan {
    /// The losses `options` does not accept — what stands in the way.
    pub fn blockers<'a>(&'a self, options: &RegroupOptions) -> Vec<&'a Loss> {
        blockers(&self.losses, options)
    }
}

impl ScatterPlan {
    /// The losses `options` does not accept — what stands in the way.
    pub fn blockers<'a>(&'a self, options: &RegroupOptions) -> Vec<&'a Loss> {
        blockers(&self.losses, options)
    }
}

fn blockers<'a>(losses: &'a [Loss], options: &RegroupOptions) -> Vec<&'a Loss> {
    losses
        .iter()
        .filter(|loss| match &loss.what {
            LossKind::Field(key) => !options.discard.iter().any(|d| d == key),
            _ => true,
        })
        .collect()
}

/// The refusal for a plan with blockers, naming every one.
fn refusal(verb: &str, blockers: &[&Loss]) -> Error {
    let lines: Vec<String> = blockers
        .iter()
        .map(|loss| match &loss.what {
            LossKind::Field(key) => format!("{}: field `{key}`", loss.record.display()),
            LossKind::Linked { from, to } => {
                format!("{}: linked to from {}", to.display(), from.display())
            }
            LossKind::Children(n) => {
                format!("{}: contains {n} document(s)", loss.record.display())
            }
        })
        .collect();
    Error::Structure(format!(
        "{verb} would lose what the other shape cannot hold — {}",
        lines.join("; ")
    ))
}

/// A field's bookkeeping, not its content — see the module docs.
fn managed(key: &str, inverse: &str, updated: Option<&str>) -> bool {
    matches!(
        key,
        "id" | "content" | "attachment" | "content_hash" | MANIFEST_KEY
    ) || key == inverse
        || Some(key) == updated
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// What gathering the attachment sidecars `cards` into one manifest over
    /// the new directory `dir` would do — read only.
    ///
    /// Refuses outright (an error, not a loss) when a card is not an
    /// attachment, its payload is missing, readable (a specimen), or already
    /// under a manifest, the cards are not all in one index, `dir` already
    /// exists, or the node's name beside it is taken. Everything else is in the plan: the moves,
    /// what is carried onto the node, and what would be lost.
    pub async fn plan_gather(&self, cards: &[PathBuf], dir: &Path) -> Result<GatherPlan> {
        let _scope = self.read_scope();
        let (spanning, inverse) = self.spanning_pair()?;
        if cards.is_empty() {
            return Err(Error::Structure("nothing to gather".into()));
        }
        let dir = link::normalize(dir);
        if self.exists(&dir).await? {
            return Err(Error::AlreadyExists(dir));
        }
        let node = sidecar_path(&dir, self.default_embed_format());
        let manifest = manifest_sibling(&node);
        for path in [&node, &manifest] {
            if self.exists(path).await? {
                return Err(Error::AlreadyExists(path.clone()));
            }
        }

        let mut parent: Option<PathBuf> = None;
        let mut files = Vec::new();
        let mut metas: Vec<(PathBuf, Mapping)> = Vec::new();
        let mut losses = Vec::new();
        let mut taken: Vec<PathBuf> = Vec::new();
        for card in cards {
            let card = link::normalize(card);
            let (_, doc) = self.load(&card).await?;
            if !doc.is_attachment() || doc.is_manifest_node() {
                return Err(Error::Structure(format!(
                    "{} is not an attachment — only sidecars are gathered",
                    card.display()
                )));
            }
            let payload = doc
                .content_path(&card)
                .ok_or_else(|| Error::Structure(format!("{} has no content", card.display())))?;
            if !self.exists(&payload).await? {
                return Err(Error::NotFound(payload));
            }
            // A manifest covers only files prov cannot read; a readable file
            // under its root would be a document there. A specimen
            // (`attach --opaque`) is shadowed by its own sidecar, which a row
            // in a manifest cannot do for it.
            if !prov_graph::document::is_opaque_payload(&payload) {
                return Err(Error::Structure(format!(
                    "{} shadows {}, a file prov can read — a manifest covers only \
                     opaque files, so a specimen keeps its own sidecar",
                    card.display(),
                    payload.display()
                )));
            }
            if self.graph().under_manifest(&payload).await? {
                return Err(Error::Structure(format!(
                    "{} is already covered by a manifest",
                    payload.display()
                )));
            }
            let up = self
                .single_target(&doc, &inverse, &card)
                .ok_or_else(|| Error::Structure(format!("{} is in no index", card.display())))?;
            match &parent {
                None => parent = Some(up),
                Some(p) if *p == up => {}
                Some(p) => {
                    return Err(Error::Structure(format!(
                        "{} is under {}, and {} under {} — gather the cards of one index",
                        cards[0].display(),
                        p.display(),
                        card.display(),
                        up.display()
                    )));
                }
            }
            let children = self
                .relations()
                .children(&fig::Value::from(&doc.meta))
                .len();
            if children > 0 {
                losses.push(Loss {
                    record: card.clone(),
                    what: LossKind::Children(children),
                });
            }
            let name = payload
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();
            let to = free_name(&dir, &name, &taken);
            taken.push(to.clone());
            files.push(GatheredFile {
                card: card.clone(),
                from: payload,
                to,
            });
            metas.push((card, doc.meta.as_mapping().cloned().unwrap_or_default()));
        }
        let parent = parent.expect("at least one card");

        // Fields: shared by every card with one value → carried onto the
        // node; anything else that is not bookkeeping → onto the row of the
        // card that has it, unless a row cannot hold it.
        let updated = self.updated_field();
        let mut keys: Vec<String> = Vec::new();
        let mut links: Vec<String> = Vec::new();
        for (_, meta) in &metas {
            for key in meta.keys() {
                if !keys.contains(key) {
                    keys.push(key.clone());
                }
            }
            for found in self
                .graph()
                .frontmatter_links(&fig::Value::from(&Value::Mapping(meta.clone())))
            {
                let head = found.address.head().to_string();
                if !links.contains(&head) {
                    links.push(head);
                }
            }
        }
        let mut carried = Vec::new();
        let mut on_rows = Vec::new();
        for key in keys {
            // Bookkeeping, or the children reported above.
            if managed(&key, &inverse, updated) || key == spanning {
                continue;
            }
            if key == "title" {
                // A written title is the row's; one that is only the file
                // name read as one is bookkeeping.
                if files
                    .iter()
                    .zip(&metas)
                    .any(|(file, (_, meta))| written_title(meta, &file.from).is_some())
                {
                    on_rows.push(key);
                }
                continue;
            }
            let first = metas[0].1.get(&key);
            if first.is_some() && metas.iter().all(|(_, m)| m.get(&key) == first) {
                carried.push(key);
            } else if links.contains(&key) || key == PATH_KEY || key == HASH_KEY {
                for (card, meta) in &metas {
                    if meta.contains_key(&key) {
                        losses.push(Loss {
                            record: card.clone(),
                            what: LossKind::Field(key.clone()),
                        });
                    }
                }
            } else {
                on_rows.push(key);
            }
        }

        // Links: anything reaching a card, other than its parent's list entry
        // and the card itself, or reaching a payload, other than its card.
        let root = self.spanning_root(&parent, &inverse).await?;
        let backlinks = self.backlinks(&root).await?;
        for file in &files {
            for backlink in backlinks.get(&file.card).into_iter().flatten() {
                let parents_entry = backlink.source == parent
                    && matches!(&backlink.site, LinkSite::Relation { field, .. } if *field == spanning);
                if backlink.source != file.card && !parents_entry {
                    losses.push(Loss {
                        record: file.card.clone(),
                        what: LossKind::Linked {
                            from: backlink.source.clone(),
                            to: file.card.clone(),
                        },
                    });
                }
            }
            for backlink in backlinks.get(&file.from).into_iter().flatten() {
                if backlink.source != file.card {
                    losses.push(Loss {
                        record: file.card.clone(),
                        what: LossKind::Linked {
                            from: backlink.source.clone(),
                            to: file.from.clone(),
                        },
                    });
                }
            }
        }

        Ok(GatherPlan {
            parent,
            node,
            manifest,
            root: dir,
            files,
            carried,
            on_rows,
            losses,
        })
    }

    /// Gather the attachment sidecars `cards` into one manifest node titled
    /// `title`, over the new directory `dir`: the payloads move into it, the
    /// sidecars go, and the node takes the first sidecar's place in their
    /// index. Refused while [`plan_gather`](Self::plan_gather) reports a loss
    /// `options` does not accept. Returns the plan carried out.
    ///
    /// A sidecar's `content_hash` becomes its row's digest, and its fields in
    /// [`on_rows`](GatherPlan::on_rows) the row's fields. When the workspace
    /// records checksums and a sidecar had none, the payload is read and
    /// hashed, so the manifest is a fixity baseline throughout or (when the
    /// workspace records none and the sidecars had none) an inventory.
    pub async fn gather(
        &mut self,
        cards: &[PathBuf],
        dir: &Path,
        title: &str,
        options: &RegroupOptions,
    ) -> Result<GatherPlan> {
        let plan = self.plan_gather(cards, dir).await?;
        let blocking = plan.blockers(options);
        if !blocking.is_empty() {
            return Err(refusal("gathering", &blocking));
        }
        let (spanning, inverse) = self.spanning_pair()?;
        let format = self.default_embed_format();

        // The rows, each with the digest its sidecar recorded or, under
        // fixity, one read now.
        let mut entries = Vec::new();
        let mut first_meta: Option<Mapping> = None;
        for file in &plan.files {
            let (_, doc) = self.load(&file.card).await?;
            let recorded = doc
                .meta
                .get("content_hash")
                .and_then(Value::as_str)
                .filter(|h| crate::fixity::is_recognized(h))
                .map(str::to_owned);
            let hash = match recorded {
                Some(hash) => Some(hash),
                None if self.fixity().is_on() => {
                    Some(crate::fixity::digest(&self.read_bytes(&file.from).await?))
                }
                None => None,
            };
            let meta = doc.meta.as_mapping().cloned().unwrap_or_default();
            let mut fields = Mapping::new();
            for key in &plan.on_rows {
                if options.discard.contains(key) {
                    continue;
                }
                let value = if key == "title" {
                    written_title(&meta, &file.from).cloned()
                } else {
                    meta.get(key).cloned()
                };
                if let Some(value) = value {
                    fields.insert(key.clone(), value);
                }
            }
            if first_meta.is_none() {
                first_meta = Some(meta);
            }
            entries.push(ManifestEntry {
                path: file
                    .to
                    .strip_prefix(&plan.root)
                    .unwrap_or(&file.to)
                    .to_path_buf(),
                hash,
                fields,
            });
        }
        // An inventory or a baseline, never half of each.
        if entries.iter().any(|e| e.hash.is_none()) {
            for entry in &mut entries {
                entry.hash = None;
            }
        }
        let root_text = format!(
            "{}/",
            link::relative(plan.manifest.parent().unwrap_or(Path::new("")), &plan.root)
        );
        let mut manifest = Manifest {
            root: root_text,
            files: entries,
        };
        manifest.sort();
        let manifest_text = prov_graph::meta::serialize_mapping(
            &manifest.to_mapping(&format!("{title} — manifest")),
            format,
        )?;

        let (parent_text, parent_doc) = self.load(&plan.parent).await?;
        let parent_title = parent_doc
            .meta
            .get("title")
            .and_then(prov_graph::title::title_text)
            .unwrap_or_else(|| link::path_to_title(&plan.parent));
        let indexes: Vec<usize> = plan
            .files
            .iter()
            .filter_map(|f| self.entry_index(&parent_doc, &spanning, &plan.parent, &f.card))
            .collect();
        let card_ids: Vec<_> = plan
            .files
            .iter()
            .filter_map(|f| self.index().id_for_path(&f.card))
            .collect();

        let mut cs = self.change();
        let up = self
            .authored_target(&inverse, &plan.node, &plan.parent, &parent_title, true)
            .await?;
        let down = self
            .authored_target(&spanning, &plan.parent, &plan.node, title, false)
            .await?;

        let mut map = Mapping::new();
        map.insert("title".into(), Value::String(title.to_string()));
        map.insert(inverse.clone(), Value::String(up));
        map.insert(
            MANIFEST_KEY.into(),
            Value::String(
                plan.manifest
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
        );
        if self.fixity().is_on() {
            map.insert(
                "content_hash".into(),
                Value::String(crate::fixity::digest(manifest_text.as_bytes())),
            );
        }
        if let Some(meta) = &first_meta {
            for key in &plan.carried {
                if let Some(value) = meta.get(key) {
                    map.insert(key.clone(), value.clone());
                }
            }
        }
        let node_text = prov_graph::meta::serialize_mapping(&map, format)?;

        // The parent's list: the node in the first card's place, the rest of
        // the cards out. Removed from the back so earlier positions hold.
        let mut editor = MetaEditor::open_or_init(&parent_text, parent_doc.carrier)?;
        let span = Segment::Key(&spanning);
        match indexes.iter().min() {
            Some(&first) => {
                editor.replace_value(&[span, Segment::Index(first)], fig::Value::Str(down))?;
                let mut rest: Vec<usize> =
                    indexes.iter().copied().filter(|&i| i != first).collect();
                rest.sort_unstable_by(|a, b| b.cmp(a));
                for index in rest {
                    editor.remove_item(&[span], index)?;
                }
            }
            None => {
                if editor
                    .append_value(&[span], fig::Value::Str(down.clone()))
                    .is_err()
                {
                    editor.set_value(&[span], fig::Value::Seq(vec![fig::Value::Str(down)]))?;
                }
            }
        }
        let parent_out = editor.render()?;

        for file in &plan.files {
            cs.rename(&file.from, &file.to);
            cs.remove(&file.card);
        }
        cs.expect_absent(&plan.node);
        cs.expect_absent(&plan.manifest);
        cs.write(&plan.manifest, manifest_text);
        cs.write(&plan.node, node_text);
        cs.write(&plan.parent, parent_out);

        for id in card_ids {
            self.index_mut().unregister(&id);
        }
        if self.identity().registration().fires_on(Trigger::Create)
            && self.index().id_for_path(&plan.node).is_none()
        {
            let id = self.mint_unique(&plan.node);
            self.index_mut().register(&id, &plan.node);
        }
        self.commit(cs).await?;
        Ok(plan)
    }

    /// What scattering the manifest node `node` back into one attachment
    /// sidecar per covered file would do — read only.
    ///
    /// With `into` `None` the node is **kept**, as the index the sidecars go
    /// under: it loses its `manifest` pointer and gains a list of them, and
    /// keeps its title, its id and every link to it — so nothing is lost.
    /// With `into` naming an index, the node is removed and the sidecars go
    /// under that index (in the node's place, when it is the node's own
    /// parent); then the node's fields are copied onto every sidecar, its
    /// title and any link to it are what would be lost.
    ///
    /// Refuses outright when `node` declares no manifest, when the directory
    /// no longer agrees with the manifest (a file missing, or one nobody
    /// listed — resolve that first, as `manifest --update` would have you), or
    /// when a sidecar's name is already taken.
    pub async fn plan_scatter(&self, node: &Path, into: Option<&Path>) -> Result<ScatterPlan> {
        let _scope = self.read_scope();
        let (spanning, inverse) = self.spanning_pair()?;
        let node = link::normalize(node);
        let Some(status) = self.manifest_status(&node).await? else {
            return Err(Error::Structure(format!(
                "{} declares no manifest",
                node.display()
            )));
        };
        if !status.agrees() {
            return Err(Error::Structure(format!(
                "{} does not agree with its directory ({} missing, {} unlisted) — \
                 bring the record up to date first",
                node.display(),
                status.missing.len(),
                status.extra.len()
            )));
        }
        let Some((manifest_doc, manifest)) = self.manifest_of(&node).await? else {
            return Err(Error::Structure(format!(
                "{} declares no manifest",
                node.display()
            )));
        };
        let format = self.default_embed_format();
        let updated = self.updated_field();
        let mut cards = Vec::new();
        let mut from_rows: Vec<String> = Vec::new();
        let mut losses = Vec::new();
        for entry in &manifest.files {
            let payload = manifest.file_path(&manifest_doc, entry);
            let card = sidecar_path(&payload, format);
            if self.exists(&card).await? {
                return Err(Error::AlreadyExists(card));
            }
            for key in entry.fields.keys() {
                // The sidecar's own bookkeeping is written by the scatter, not
                // taken from a row, which has no say over it.
                if managed(key, &inverse, updated) || *key == spanning {
                    losses.push(Loss {
                        record: payload.clone(),
                        what: LossKind::Field(key.clone()),
                    });
                } else if !from_rows.contains(key) {
                    from_rows.push(key.clone());
                }
            }
            cards.push((card, payload));
        }

        let (_, doc) = self.load(&node).await?;
        let (keeps_node, into_path) = match into {
            None => (true, node.clone()),
            Some(index) => (false, link::normalize(index)),
        };
        let mut carried = Vec::new();
        if !keeps_node {
            let fields = doc.meta.as_mapping().cloned().unwrap_or_default();
            for key in fields.keys() {
                if managed(key, &inverse, updated) || *key == spanning {
                    continue;
                }
                if key == "title" {
                    losses.push(Loss {
                        record: node.clone(),
                        what: LossKind::Field(key.clone()),
                    });
                } else {
                    carried.push(key.clone());
                }
            }
            let children = self
                .relations()
                .children(&fig::Value::from(&doc.meta))
                .len();
            if children > 0 {
                losses.push(Loss {
                    record: node.clone(),
                    what: LossKind::Children(children),
                });
            }
            let parent = self.single_target(&doc, &inverse, &node);
            let root = self.spanning_root(&node, &inverse).await?;
            for backlink in self.backlinks_to(&root, &node).await? {
                let parents_entry = Some(&backlink.source) == parent.as_ref()
                    && matches!(&backlink.site, LinkSite::Relation { field, .. } if *field == spanning);
                if backlink.source != node && !parents_entry {
                    losses.push(Loss {
                        record: node.clone(),
                        what: LossKind::Linked {
                            from: backlink.source,
                            to: node.clone(),
                        },
                    });
                }
            }
        }

        Ok(ScatterPlan {
            node,
            manifest: manifest_doc,
            into: into_path,
            keeps_node,
            cards,
            carried,
            from_rows,
            losses,
        })
    }

    /// Scatter the manifest node `node` into one attachment sidecar per
    /// covered file, in the manifest's order — under the node itself, kept as
    /// an index, or under `into`. The files stay where they are. Refused while
    /// [`plan_scatter`](Self::plan_scatter) reports a loss `options` does not
    /// accept. Returns the plan carried out.
    ///
    /// Each row's digest becomes its sidecar's `content_hash`, and its fields
    /// the sidecar's — a row's `title` in place of the file name's. Under
    /// fixity a row with none is read and hashed, as `attach` would.
    pub async fn scatter(
        &mut self,
        node: &Path,
        into: Option<&Path>,
        options: &RegroupOptions,
    ) -> Result<ScatterPlan> {
        let plan = self.plan_scatter(node, into).await?;
        let blocking = plan.blockers(options);
        if !blocking.is_empty() {
            return Err(refusal("scattering", &blocking));
        }
        let (spanning, inverse) = self.spanning_pair()?;
        let format = self.default_embed_format();
        let Some((manifest_doc, manifest)) = self.manifest_of(&plan.node).await? else {
            return Err(Error::Structure(format!(
                "{} declares no manifest",
                plan.node.display()
            )));
        };
        // The rows in the plan's order — the manifest's own — so the digests
        // line up with the cards.
        debug_assert_eq!(manifest.files.len(), plan.cards.len());
        debug_assert!(
            plan.cards
                .iter()
                .zip(&manifest.files)
                .all(|((_, payload), entry)| {
                    *payload == manifest.file_path(&manifest_doc, entry)
                })
        );
        let rows: Vec<(Option<String>, Mapping)> = manifest
            .files
            .iter()
            .map(|e| (e.hash.clone(), e.fields.clone()))
            .collect();
        let (node_text, node_doc) = self.load(&plan.node).await?;
        let (into_text, into_doc) = if plan.keeps_node {
            (node_text.clone(), node_doc.clone())
        } else {
            self.load(&plan.into).await?
        };
        let into_title = into_doc
            .meta
            .get("title")
            .and_then(prov_graph::title::title_text)
            .unwrap_or_else(|| link::path_to_title(&plan.into));
        let node_parent = self.single_target(&node_doc, &inverse, &plan.node);
        let node_id = self.index().id_for_path(&plan.node);

        let mut cs = self.change();
        let mut downs = Vec::new();
        let mut sidecars = Vec::new();
        for ((card, payload), (hash, fields)) in plan.cards.iter().zip(rows) {
            let title = fields
                .get("title")
                .and_then(prov_graph::title::title_text)
                .unwrap_or_else(|| link::path_to_title(payload));
            let up = self
                .authored_target(&inverse, card, &plan.into, &into_title, true)
                .await?;
            downs.push(
                self.authored_target(&spanning, &plan.into, card, &title, false)
                    .await?,
            );
            let mut map = Mapping::new();
            map.insert("title".into(), Value::String(title));
            map.insert(inverse.clone(), Value::String(up));
            map.insert(
                "content".into(),
                Value::String(
                    payload
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                        .to_string(),
                ),
            );
            map.insert("attachment".into(), Value::Bool(true));
            let hash = match hash {
                Some(hash) => Some(hash),
                None if self.fixity().is_on() => {
                    Some(crate::fixity::digest(&self.read_bytes(payload).await?))
                }
                None => None,
            };
            if let Some(hash) = hash {
                map.insert("content_hash".into(), Value::String(hash));
            }
            for key in &plan.carried {
                if let Some(value) = node_doc.meta.get(key) {
                    map.insert(key.clone(), value.clone());
                }
            }
            // The row's own, last: it is about this file, where a carried
            // field was about all of them.
            for (key, value) in fields {
                if key != "title" && plan.from_rows.contains(&key) {
                    map.insert(key, value);
                }
            }
            sidecars.push((
                card.clone(),
                prov_graph::meta::serialize_mapping(&map, format)?,
            ));
        }
        let down_values: Vec<fig::Value> = downs.into_iter().map(fig::Value::Str).collect();

        cs.remove(&plan.manifest);
        if plan.keeps_node {
            // The node becomes the index: no manifest, no digest of one, and
            // the sidecars as its list.
            let mut editor = MetaEditor::open(
                &node_text,
                node_doc.carrier.ok_or_else(|| {
                    Error::Structure(format!("{} has no metadata block", plan.node.display()))
                })?,
            )?;
            editor.delete(&[Segment::Key(MANIFEST_KEY)])?;
            if node_doc.meta.get("content_hash").is_some() {
                editor.delete(&[Segment::Key("content_hash")])?;
            }
            // After whatever the node already held, which stays.
            let mut list: Vec<fig::Value> = node_doc
                .meta
                .get(&spanning)
                .map(Value::link_strings)
                .unwrap_or_default()
                .into_iter()
                .map(fig::Value::Str)
                .collect();
            list.extend(down_values);
            editor.set_value(&[Segment::Key(&spanning)], fig::Value::Seq(list))?;
            cs.write(&plan.node, editor.render()?);
        } else {
            cs.remove(&plan.node);
            // The target index: the sidecars in the node's place when it is
            // the node's own parent, else after what it holds.
            let mut editor = MetaEditor::open_or_init(&into_text, into_doc.carrier)?;
            let span = Segment::Key(&spanning);
            let at = (node_parent.as_ref() == Some(&plan.into))
                .then(|| self.entry_index(&into_doc, &spanning, &plan.into, &plan.node))
                .flatten();
            let existing = into_doc
                .meta
                .get(&spanning)
                .map(Value::link_strings)
                .unwrap_or_default();
            let mut list: Vec<fig::Value> = existing.into_iter().map(fig::Value::Str).collect();
            match at {
                Some(i) => {
                    list.splice(i..=i, down_values);
                }
                None => list.extend(down_values),
            }
            editor.set_value(&[span], fig::Value::Seq(list))?;
            cs.write(&plan.into, editor.render()?);
            // The node's old parent, when it is somewhere else: its entry goes.
            if let Some(parent) = node_parent.filter(|p| *p != plan.into) {
                let (parent_text, parent_doc) = self.load(&parent).await?;
                if let (Some(index), Some(carrier)) = (
                    self.entry_index(&parent_doc, &spanning, &parent, &plan.node),
                    parent_doc.carrier,
                ) {
                    let mut editor = MetaEditor::open(&parent_text, carrier)?;
                    editor.remove_item(&[Segment::Key(&spanning)], index)?;
                    cs.write(&parent, editor.render()?);
                }
            }
            if let Some(id) = node_id {
                self.index_mut().unregister(&id);
            }
        }
        for (card, text) in sidecars {
            cs.expect_absent(&card);
            cs.write(&card, text);
            if self.identity().registration().fires_on(Trigger::Create)
                && self.index().id_for_path(&card).is_none()
            {
                let id = self.mint_unique(&card);
                self.index_mut().register(&id, &card);
            }
        }
        self.commit(cs).await?;
        Ok(plan)
    }
}

/// The title a sidecar's person wrote — `None` when it has none, or only the
/// one its payload's file name reads as.
fn written_title<'m>(meta: &'m Mapping, payload: &Path) -> Option<&'m Value> {
    let title = meta.get("title")?;
    let derived = link::path_to_title(payload);
    (prov_graph::title::title_text(title).as_deref() != Some(derived.as_str())).then_some(title)
}

/// `name` under `dir`, suffixed (`photo-2.jpg`) when an earlier file in the
/// same gather already took it — two cameras both say `IMG_0001.jpg`. The
/// directory is new, so only the gather's own names can collide.
fn free_name(dir: &Path, name: &str, taken: &[PathBuf]) -> PathBuf {
    let first = dir.join(name);
    if !taken.contains(&first) {
        return first;
    }
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = path.extension().and_then(|s| s.to_str());
    (2..)
        .map(|n| match ext {
            Some(ext) => dir.join(format!("{stem}-{n}.{ext}")),
            None => dir.join(format!("{stem}-{n}")),
        })
        .find(|p| !taken.contains(p))
        .expect("an unbounded range finds a free name")
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use prov_graph::index::IdIndex;

    /// An index holding a page and three photographs, each attached the
    /// ordinary way — a sidecar, a digest, a place in the list.
    fn album_tree(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\nhi\n",
        );
        for (name, byte) in [("a.jpg", 1u8), ("b.jpg", 2), ("c.jpg", 3)] {
            write(&dir, name, [0xff, 0xd8, byte]);
            block_on(ws(&dir).attach(Path::new(name), Path::new("index.md"))).unwrap();
        }
        dir
    }

    fn cards(names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|n| PathBuf::from(format!("{n}.yaml")))
            .collect()
    }

    #[test]
    fn gathering_makes_one_node_over_a_directory_in_the_first_card_s_place() {
        let dir = album_tree("gather");
        let digest_a = read(&dir, "a.jpg.yaml")
            .lines()
            .find(|l| l.starts_with("content_hash"))
            .unwrap()
            .to_string();

        let plan = block_on(ws(&dir).gather(
            &cards(&["a.jpg", "b.jpg"]),
            Path::new("album"),
            "Album",
            &RegroupOptions::default(),
        ))
        .unwrap();
        assert_eq!(plan.node, PathBuf::from("album.yaml"));
        assert!(plan.losses.is_empty(), "{:?}", plan.losses);

        assert!(dir.join("album/a.jpg").exists() && dir.join("album/b.jpg").exists());
        assert!(!dir.join("a.jpg").exists() && !dir.join("a.jpg.yaml").exists());
        let manifest = read(&dir, "album.manifest.yaml");
        assert!(manifest.contains("root: album/"), "{manifest}");
        assert!(
            manifest.contains(digest_a.trim_start_matches("content_hash: ")),
            "the card's digest is the row's: {manifest}"
        );
        let index = read(&dir, "index.md");
        let order: Vec<&str> = index.lines().filter(|l| l.starts_with("- ")).collect();
        assert_eq!(order.len(), 3, "{index}");
        assert!(order[0].contains("note.md"), "{index}");
        assert!(
            order[1].contains("album.yaml"),
            "the node in the first card's place: {index}"
        );
        assert!(order[2].contains("c.jpg.yaml"), "{index}");
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_shared_field_moves_to_the_node_and_one_card_s_onto_its_row() {
        let dir = album_tree("gather-fields");
        for name in ["a.jpg.yaml", "b.jpg.yaml"] {
            let text = read(&dir, name) + "audience: family\n";
            write(&dir, name, text);
        }
        let text = read(&dir, "a.jpg.yaml").replace("title: A\n", "title: Mum at the lake\n")
            + "date_of_document: 2025-05-02\n";
        write(&dir, "a.jpg.yaml", text);

        let plan = block_on(ws(&dir).plan_gather(&cards(&["a.jpg", "b.jpg"]), Path::new("album")))
            .unwrap();
        assert_eq!(plan.carried, ["audience"]);
        assert_eq!(plan.on_rows, ["title", "date_of_document"]);
        assert!(plan.losses.is_empty(), "{:?}", plan.losses);

        block_on(ws(&dir).gather(
            &cards(&["a.jpg", "b.jpg"]),
            Path::new("album"),
            "Album",
            &RegroupOptions::default(),
        ))
        .unwrap();
        assert!(read(&dir, "album.yaml").contains("audience: family"));
        let (_, manifest) = block_on(ws(&dir).manifest_of(Path::new("album.yaml")))
            .unwrap()
            .unwrap();
        let a = &manifest.files[0].fields;
        assert_eq!(
            a.keys().collect::<Vec<_>>(),
            ["title", "date_of_document"],
            "{a:?}"
        );
        assert_eq!(a["title"], Value::String("Mum at the lake".into()));
        assert!(
            manifest.files[1].fields.is_empty(),
            "b's title was only its file name: {:?}",
            manifest.files[1].fields
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_discarded_field_stays_off_the_row() {
        let dir = album_tree("gather-discard");
        let text = read(&dir, "a.jpg.yaml") + "date_of_document: 2025-05-02\n";
        write(&dir, "a.jpg.yaml", text);
        block_on(ws(&dir).gather(
            &cards(&["a.jpg", "b.jpg"]),
            Path::new("album"),
            "Album",
            &RegroupOptions {
                discard: vec!["date_of_document".into()],
            },
        ))
        .unwrap();
        let manifest = read(&dir, "album.manifest.yaml");
        assert!(!manifest.contains("date_of_document"), "{manifest}");
    }

    /// A row is not read for links, so one carried there would be a link no
    /// rename rewrites and no `check` sees: it is a loss, as before.
    #[test]
    fn a_field_holding_a_link_cannot_go_onto_a_row() {
        let dir = album_tree("gather-link-field");
        let text = read(&dir, "a.jpg.yaml") + "source: '[Note](/note.md)'\n";
        write(&dir, "a.jpg.yaml", text);
        let linked = |dir: &Path| {
            Workspace::builder(StdFs)
                .root(dir)
                .references(vec![prov_graph::field::FieldPath::parse("source")])
                .build()
        };
        let plan =
            block_on(linked(&dir).plan_gather(&cards(&["a.jpg", "b.jpg"]), Path::new("album")))
                .unwrap();
        assert!(plan.on_rows.is_empty(), "{:?}", plan.on_rows);
        assert_eq!(
            plan.losses,
            [Loss {
                record: "a.jpg.yaml".into(),
                what: LossKind::Field("source".into())
            }]
        );
    }

    /// A row's fields go back onto its sidecar, its title in place of the
    /// file name's and over a field the node carries; one naming the
    /// sidecar's own bookkeeping is a loss.
    #[test]
    fn scattering_puts_a_row_s_fields_back_on_its_sidecar() {
        let dir = tempdir("scatter-fields");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(&dir, "photos/a.jpg", [0xff, 0xd8, 1]);
        write(&dir, "photos/b.jpg", [0xff, 0xd8, 2]);
        let manifest = "title: Photos — manifest\nroot: photos/\nfiles:\n\
                        - path: a.jpg\n  title: Mum at the lake\n  audience: just us\n\
                        - path: b.jpg\n";
        write(&dir, "photos.manifest.yaml", manifest);
        write(
            &dir,
            "photos.yaml",
            "title: Photos\npart_of: index.md\nmanifest: photos.manifest.yaml\naudience: family\n",
        );
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- photos.yaml\n---\n",
        );

        let plan =
            block_on(ws(&dir).plan_scatter(Path::new("photos.yaml"), Some(Path::new("index.md"))))
                .unwrap();
        assert_eq!(plan.from_rows, ["title", "audience"]);
        block_on(ws(&dir).scatter(
            Path::new("photos.yaml"),
            Some(Path::new("index.md")),
            &RegroupOptions {
                discard: vec!["title".into()],
            },
        ))
        .unwrap();
        let a = read(&dir, "photos/a.jpg.yaml");
        assert!(a.contains("title: Mum at the lake"), "{a}");
        assert!(
            a.contains("audience: just us") && !a.contains("family"),
            "{a}"
        );
        let b = read(&dir, "photos/b.jpg.yaml");
        assert!(
            b.contains("title: B\n") && b.contains("audience: family"),
            "{b}"
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);

        let dir = tempdir("scatter-managed");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(&dir, "photos/a.jpg", [0xff, 0xd8, 1]);
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        let text = read(&dir, "photos.manifest.yaml").replace(
            "- path: a.jpg\n",
            "- path: a.jpg\n  content: elsewhere.jpg\n",
        );
        write(&dir, "photos.manifest.yaml", text);
        let plan = block_on(ws(&dir).plan_scatter(Path::new("photos.yaml"), None)).unwrap();
        assert_eq!(
            plan.losses,
            [Loss {
                record: "photos/a.jpg".into(),
                what: LossKind::Field("content".into())
            }]
        );
    }

    #[test]
    fn a_page_embedding_a_photograph_blocks_the_gather_whatever_is_accepted() {
        let dir = album_tree("gather-linked");
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\n![](b.jpg)\n",
        );
        let plan = block_on(ws(&dir).plan_gather(&cards(&["a.jpg", "b.jpg"]), Path::new("album")))
            .unwrap();
        assert_eq!(
            plan.losses,
            [Loss {
                record: "b.jpg.yaml".into(),
                what: LossKind::Linked {
                    from: "note.md".into(),
                    to: "b.jpg".into()
                }
            }]
        );
        assert!(!plan.losses[0].what.discardable());
    }

    #[test]
    fn cards_from_two_indexes_are_refused() {
        let dir = album_tree("gather-two");
        write(
            &dir,
            "other.md",
            "---\ntitle: Other\npart_of: index.md\n---\n",
        );
        let text = read(&dir, "index.md").replace("- note.md", "- note.md\n- other.md");
        write(&dir, "index.md", text);
        write(&dir, "d.jpg", [0xff, 0xd8, 4]);
        block_on(ws(&dir).attach(Path::new("d.jpg"), Path::new("other.md"))).unwrap();
        let err = block_on(ws(&dir).plan_gather(&cards(&["a.jpg", "d.jpg"]), Path::new("album")))
            .unwrap_err();
        assert!(err.to_string().contains("one index"), "{err}");
    }

    /// The default scatter keeps the node, as the index: same path, title and
    /// id, no manifest, a sidecar per file in the manifest's order.
    #[test]
    fn scattering_turns_the_node_into_an_index_of_sidecars() {
        let dir = tempdir("scatter");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(&dir, "photos/a.jpg", [0xff, 0xd8, 1]);
        write(&dir, "photos/b.jpg", [0xff, 0xd8, 2]);
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        let plan =
            block_on(ws(&dir).scatter(Path::new("photos.yaml"), None, &RegroupOptions::default()))
                .unwrap();
        assert!(plan.keeps_node);
        assert!(!dir.join("photos.manifest.yaml").exists());
        let node = read(&dir, "photos.yaml");
        assert!(!node.contains("manifest:"), "{node}");
        assert!(
            node.contains("photos/a.jpg.yaml") && node.contains("photos/b.jpg.yaml"),
            "{node}"
        );
        let card = read(&dir, "photos/a.jpg.yaml");
        assert!(
            card.contains("content: a.jpg") && card.contains("content_hash:"),
            "{card}"
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    /// Scattered into the node's own parent, the sidecars take its place; its
    /// fields go onto each, and its title is the one thing asked about.
    #[test]
    fn scattering_into_the_parent_replaces_the_node_and_carries_its_fields() {
        let dir = tempdir("scatter-into");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\n",
        );
        write(&dir, "photos/a.jpg", [0xff, 0xd8, 1]);
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        let text = read(&dir, "photos.yaml") + "audience: family\n";
        write(&dir, "photos.yaml", text);

        let plan =
            block_on(ws(&dir).plan_scatter(Path::new("photos.yaml"), Some(Path::new("index.md"))))
                .unwrap();
        assert_eq!(plan.carried, ["audience"]);
        assert_eq!(
            plan.blockers(&RegroupOptions::default())
                .iter()
                .map(|l| &l.what)
                .collect::<Vec<_>>(),
            [&LossKind::Field("title".into())]
        );
        block_on(ws(&dir).scatter(
            Path::new("photos.yaml"),
            Some(Path::new("index.md")),
            &RegroupOptions {
                discard: vec!["title".into()],
            },
        ))
        .unwrap();
        assert!(!dir.join("photos.yaml").exists());
        let index = read(&dir, "index.md");
        let order: Vec<&str> = index.lines().filter(|l| l.starts_with("- ")).collect();
        assert_eq!(order.len(), 2, "{index}");
        assert!(
            order[1].contains("photos/a.jpg.yaml"),
            "the card in the node's place: {index}"
        );
        assert!(read(&dir, "photos/a.jpg.yaml").contains("audience: family"));
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_directory_that_has_drifted_is_not_scattered() {
        let dir = tempdir("scatter-drift");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(&dir, "photos/a.jpg", [0xff, 0xd8, 1]);
        block_on(ws(&dir).attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();
        std::fs::remove_file(dir.join("photos/a.jpg")).unwrap();
        let err = block_on(ws(&dir).plan_scatter(Path::new("photos.yaml"), None)).unwrap_err();
        assert!(err.to_string().contains("1 missing"), "{err}");
    }

    /// Where the index links its children by id — as an application minting
    /// ids does — the node still takes the first card's entry, the retired
    /// cards' ids leave the registry, and a scatter mints the new sidecars'.
    #[test]
    fn ids_are_retired_and_minted_on_the_way() {
        let dir = tempdir("gather-ids");
        // One workspace throughout: its registry is in memory, and a second
        // built over the same directory would mint the same ids again.
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .identity(Minter::eager(7))
            .index(FileIndex::new(fig::Format::Yaml))
            .id_links(true)
            .build();
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        for (name, byte) in [("a.jpg", 1u8), ("b.jpg", 2)] {
            write(&dir, name, [0xff, 0xd8, byte]);
            block_on(w.attach(Path::new(name), Path::new("index.md"))).unwrap();
        }
        let card_ids: Vec<_> = ["a.jpg.yaml", "b.jpg.yaml"]
            .iter()
            .map(|c| w.index().id_for_path(Path::new(c)).expect("a card id"))
            .collect();

        block_on(w.gather(
            &cards(&["a.jpg", "b.jpg"]),
            Path::new("album"),
            "Album",
            &RegroupOptions::default(),
        ))
        .unwrap();
        let index = read(&dir, "index.md");
        let node_id = w
            .index()
            .id_for_path(Path::new("album.yaml"))
            .expect("a node id");
        assert_eq!(
            index.matches("id:").count(),
            1,
            "one entry, the node's: {index}"
        );
        assert!(index.contains(&format!("id:{}", node_id.0)), "{index}");
        for id in &card_ids {
            assert_eq!(w.index().resolve(id), None, "{id:?} retired");
        }
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        block_on(w.scatter(Path::new("album.yaml"), None, &RegroupOptions::default())).unwrap();
        assert_eq!(
            w.index().id_for_path(Path::new("album.yaml")),
            Some(node_id),
            "the node kept its id"
        );
        for card in ["album/a.jpg.yaml", "album/b.jpg.yaml"] {
            let id = w
                .index()
                .id_for_path(Path::new(card))
                .expect("a new card id");
            assert!(read(&dir, "album.yaml").contains(&id.0), "{card}");
        }
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    /// Gather, then scatter in place: every photograph is a sidecar again,
    /// now under the album, with what was said about it, and the workspace
    /// checks clean throughout.
    #[test]
    fn a_gather_scatters_back() {
        let dir = album_tree("round-trip");
        let text = read(&dir, "b.jpg.yaml").replace("title: B\n", "title: The dog\n")
            + "date_of_document: 2025-05-02\n";
        write(&dir, "b.jpg.yaml", text);
        block_on(ws(&dir).gather(
            &cards(&["a.jpg", "b.jpg", "c.jpg"]),
            Path::new("album"),
            "Album",
            &RegroupOptions::default(),
        ))
        .unwrap();
        block_on(ws(&dir).scatter(Path::new("album.yaml"), None, &RegroupOptions::default()))
            .unwrap();
        for name in ["a", "b", "c"] {
            assert!(dir.join(format!("album/{name}.jpg.yaml")).exists());
        }
        let b = read(&dir, "album/b.jpg.yaml");
        assert!(
            b.contains("title: The dog") && b.contains("date_of_document: 2025-05-02"),
            "{b}"
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_specimen_card_is_not_gathered() {
        // A manifest covers files prov cannot read. A specimen's payload is one
        // it can, kept unread only by its own sidecar — a row cannot do that.
        let dir = album_tree("gather-specimen");
        attach_specimen(&dir, "sample.md", "index.md");

        let err =
            block_on(ws(&dir).plan_gather(&cards(&["a.jpg", "sample.md"]), Path::new("album")))
                .unwrap_err();
        assert!(err.to_string().contains("only opaque files"), "{err}");
        assert!(!dir.join("album").exists());
    }
}
