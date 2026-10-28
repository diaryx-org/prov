//! Owned payloads — an attachment whose metadata lives in the document that
//! embeds it.
//!
//! [`attach`](crate::attach) gives an opaque file a sidecar of its own: a node,
//! with an id, a place in the spine and a row in every view. That is the
//! shape for a record — a scan filed into an archive. It is the wrong shape
//! for a photograph dropped into a journal entry, which is part of the entry's
//! text and not a document beside it.
//!
//! A field declared **`type: payload`** is the other shape. It is a `ref` in
//! every respect — censused, checked, rewritten when its target moves — and,
//! beyond that, its target is a file the document **owns**:
//!
//! - it **travels**: a move of the document to another directory takes each
//!   payload under the document's own directory with it, at the same path
//!   relative to the document ([`rename`](crate::Workspace::rename));
//! - it **goes with its owner**: [`delete`](crate::Workspace::delete) removes
//!   it, and records it beside the document;
//! - it is **hashed**: a `content_hash` written beside the value, in the same
//!   mapping, covers the file's bytes as a sidecar's covers its payload's, and
//!   `check` verifies it;
//! - it has **one owner**: two values owning one file is a finding.
//!
//! The rest of a row — a date, the people in a photograph — is carried, as any
//! field nobody declared is, and declared like any other field when a
//! workspace wants it governed. A row is not a node: no id, no inverse, no
//! place in the spanning tree. A file something else must cite by id should be
//! a card, with a sidecar.

use std::path::{Path, PathBuf};

use prov_graph::error::{Error, Result};
use prov_graph::field::{self, Address, FieldPath, Navigate};
use prov_graph::fs::ReadStorage;
use prov_graph::graph::LinkSite;
use prov_graph::index::IdIndex;
use prov_graph::link::{self, Link};
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;

/// The key a row records its payload's checksum at, beside the value — the
/// key a sidecar uses, so a row and a card say the same thing the same way.
pub const HASH_KEY: &str = "content_hash";

/// One file a document owns, as its metadata names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedPayload {
    /// Where the value is written: `attachments[0].content`.
    pub address: Address,
    /// The value as written.
    pub raw: String,
    /// The file it names, workspace-relative.
    pub target: PathBuf,
    /// The checksum recorded beside the value, at its own address, when the
    /// row records one.
    pub hash: Option<(Address, String)>,
}

/// Every payload the document at `doc` owns through the fields `payloads`
/// declares, in field order and then document order. A value that names no
/// path — a URL, an `id:` — owns nothing and is left to the `ref` it also is.
pub fn owned_payloads(payloads: &[FieldPath], doc: &Path, meta: &fig::Value) -> Vec<OwnedPayload> {
    let mut out = Vec::new();
    for path in payloads {
        for (address, raw) in field::strings_at(meta, path) {
            let parsed = Link::parse(&raw);
            if !parsed.is_path_target() {
                continue;
            }
            let target = link::resolve(doc, parsed.addressed_target());
            let hash = address.sibling(HASH_KEY).and_then(|at| {
                let recorded = field::strings_at(meta, &FieldPath::parse(&at.to_string()))
                    .into_iter()
                    .next()?
                    .1;
                Some((at, recorded))
            });
            out.push(OwnedPayload {
                address,
                raw,
                target,
                hash,
            });
        }
    }
    out
}

impl<FS: ReadStorage, Id, Ix: IdIndex> Workspace<FS, Id, Ix> {
    /// Every payload the document at `doc` owns — see [`owned_payloads`].
    pub async fn owned_payloads(&self, doc: &Path) -> Result<Vec<OwnedPayload>> {
        let doc = link::normalize(doc);
        if self.payloads().is_empty() {
            return Ok(Vec::new());
        }
        let (_, parsed) = self.load(&doc).await?;
        Ok(owned_payloads(
            self.payloads(),
            &doc,
            &fig::Value::from(&parsed.meta),
        ))
    }

    /// The `type: payload` field a new row goes in: the one named, or the only
    /// one declared. It must be a path of rows (`attachments[].content`), since
    /// only that shape says where a row goes.
    fn row_field(&self, named: Option<&str>) -> Result<FieldPath> {
        let rows: Vec<&FieldPath> = self
            .payloads()
            .iter()
            .filter(|p| p.row().is_some())
            .collect();
        let chosen = match named {
            Some(name) => {
                let wanted = FieldPath::parse(name);
                rows.into_iter().find(|p| **p == wanted).ok_or_else(|| {
                    Error::Structure(format!(
                        "`{name}` is not declared `type: payload` as a path of rows \
                         (`list[].key`)"
                    ))
                })?
            }
            None => match rows.as_slice() {
                [one] => *one,
                [] => {
                    return Err(Error::Structure(
                        "no field is declared `type: payload` as a path of rows — \
                         declare one, e.g. `fields: { attachments[].content: { type: payload } }`"
                            .into(),
                    ));
                }
                several => {
                    let names: Vec<String> = several.iter().map(|p| p.to_string()).collect();
                    return Err(Error::Structure(format!(
                        "several fields are declared `type: payload` ({}); name one",
                        names.join(", ")
                    )));
                }
            },
        };
        Ok(chosen.clone())
    }
}

impl<FS: ReadStorage, Id, Ix: IdIndex> Workspace<FS, Id, Ix> {
    /// The checksums the rows of the document at `doc` are owed: for each
    /// owned payload whose value sits in a row, the address of its
    /// `content_hash` and the digest of the file's bytes now — where the row
    /// records none, or records one that no longer matches. Empty when the
    /// workspace records no checksums.
    ///
    /// Skips what [`check`](Self::check) skips: a file that is missing (a
    /// broken link, not a checksum to write), a digest this build cannot
    /// compute (never overwritten), and a value that is not in a row (there is
    /// no mapping to write a sibling into).
    pub async fn payload_restamps(&self, doc: &Path) -> Result<Vec<(Address, String)>> {
        if !self.fixity().is_on() {
            return Ok(Vec::new());
        }
        let mut owed = Vec::new();
        for owned in self.owned_payloads(doc).await? {
            let Some(at) = owned.address.sibling(HASH_KEY) else {
                continue;
            };
            if let Some((_, recorded)) = &owned.hash
                && !crate::fixity::is_recognized(recorded)
            {
                continue;
            }
            let Ok(digest) = self.file_digest(&owned.target).await else {
                continue;
            };
            if owned.hash.as_ref().map(|(_, h)| h) != Some(&digest) {
                owed.push((at, digest));
            }
        }
        Ok(owed)
    }
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Write every checksum [`payload_restamps`](Self::payload_restamps) says
    /// the rows of `doc` are owed, in one write, and return where each went.
    /// Accepts the files' current bytes as intended, which is what `prov
    /// stamp` is for; nothing else calls it, so an ordinary save never reads
    /// a document's pictures.
    pub async fn restamp_payloads(&mut self, doc: &Path) -> Result<Vec<Address>> {
        let doc = link::normalize(doc);
        let owed = self.payload_restamps(&doc).await?;
        if owed.is_empty() {
            return Ok(Vec::new());
        }
        let (text, parsed) = self.load(&doc).await?;
        let carrier = parsed
            .carrier
            .ok_or_else(|| Error::Structure(format!("{} has no metadata", doc.display())))?;
        let mut editor = MetaEditor::open(&text, carrier)?;
        for (at, digest) in &owed {
            editor.set_value(&at.segments(), fig::Value::Str(digest.clone()))?;
        }
        let out = editor.render()?;
        let mut cs = self.change();
        cs.expect(&doc, text);
        cs.write(&doc, out);
        self.commit(cs).await?;
        Ok(owed.into_iter().map(|(at, _)| at).collect())
    }

    /// Attach the opaque file at `payload` to the document at `doc` **inline**:
    /// append a row to `doc`'s own metadata, in the `type: payload` field
    /// `field` names (or the only one declared), rather than minting a sidecar.
    /// The row holds the link to the payload and, when the workspace records
    /// checksums, a `content_hash` of its bytes. Returns the address of the
    /// new value (`attachments[2].content`).
    ///
    /// Refuses what [`attach`](Self::attach) refuses of the payload itself —
    /// missing, readable as a document, covered by a manifest, or already
    /// given a sidecar — and a payload `doc` already owns.
    pub async fn attach_inline(
        &mut self,
        payload: &Path,
        doc: &Path,
        field: Option<&str>,
    ) -> Result<Address> {
        let payload = link::normalize(payload);
        let doc = link::normalize(doc);
        let path = self.row_field(field)?;
        let Some((list, key)) = path.row() else {
            unreachable!("row_field only returns paths of rows");
        };
        self.check_attach(&payload, false).await?;

        let (text, parsed) = self.load(&doc).await?;
        let meta = fig::Value::from(&parsed.meta);
        if owned_payloads(self.payloads(), &doc, &meta)
            .iter()
            .any(|owned| owned.target == payload)
        {
            return Err(Error::Structure(format!(
                "{} already owns {}",
                doc.display(),
                payload.display()
            )));
        }

        let value_address = list.to_string();
        let site = LinkSite::Field {
            path: value_address.clone(),
        };
        let spelled = link::path_text(self.site_path_style(&site), &doc, &payload);
        let mut row = vec![(fig::Value::Str(key.to_string()), fig::Value::Str(spelled))];
        if self.fixity().is_on() {
            row.push((
                fig::Value::Str(HASH_KEY.to_string()),
                fig::Value::Str(self.file_digest(&payload).await?),
            ));
        }
        let row = fig::Value::Map(row);

        let existing = field::values_at(&meta, &FieldPath::parse(&value_address))
            .into_iter()
            .next()
            .map(|(_, v)| v.items().map_or(0, <[fig::Value]>::len));
        let mut editor = MetaEditor::open_or_init(&text, parsed.carrier)?;
        let segments = list.segments();
        let index = match existing {
            Some(len) => {
                editor.append_value(&segments, row)?;
                len
            }
            None => {
                editor.set_value(&segments, fig::Value::Seq(vec![row]))?;
                0
            }
        };
        let out = editor.render()?;

        let mut cs = self.change();
        cs.expect(&doc, text);
        cs.write(&doc, out);
        self.commit(cs).await?;
        Ok(Address::parse(&format!("{value_address}[{index}].{key}"))
            .expect("a row address has no `[]`"))
    }
}

/// A link that keeps a card from becoming a row: a reference to the card
/// itself, by path or by id, that a demotion would leave pointing at nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardLink {
    /// The document that holds the link.
    pub source: PathBuf,
    /// Where in it the link is written.
    pub site: LinkSite,
    /// Whether it names the card by `id:`.
    pub by_id: bool,
}

/// Remove item `index` of the `len`-item list at `path`, and the list with its
/// last item. Where the format's editor will not delete the key — fig's
/// refuses a figl `>` block list, and cannot leave one with no items either —
/// the list is replaced by `[]`, as `remove_item` leaves one elsewhere.
fn remove_entry(
    editor: &mut MetaEditor,
    path: &[fig::Segment],
    index: usize,
    len: usize,
) -> Result<()> {
    if len > 1 {
        return editor.remove_item(path, index);
    }
    if editor.delete(path).is_ok() {
        return Ok(());
    }
    editor.replace_value(path, fig::Value::Seq(Vec::new()))
}

/// The keys a sidecar holds as a node rather than as facts about its payload,
/// beside the spanning inverse and `content`: a row has no id, and is not an
/// attachment sidecar.
const NODE_KEYS: [&str; 2] = ["id", "attachment"];

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// The documents that link to the attachment card at `card` itself — by
    /// path or by id, not to its payload — other than its parent's spanning
    /// entry for it: every link [`demote`](Self::demote) refuses over.
    pub async fn card_links(&self, card: &Path) -> Result<Vec<CardLink>> {
        let card = link::normalize(card);
        let (spanning, inverse) = self.spanning_pair()?;
        let (_, doc) = self.load(&card).await?;
        let parent = self.single_target(&doc, &inverse, &card);
        let root = self.spanning_root(&card, &inverse).await?;
        let mut out = Vec::new();
        for back in self.backlinks_to_node(&root, &card).await? {
            if back.via_payload || back.source == card {
                continue;
            }
            let spanning_entry = matches!(
                &back.site,
                LinkSite::Relation { field, .. } if *field == spanning
            );
            if spanning_entry && Some(&back.source) == parent.as_ref() {
                continue;
            }
            out.push(CardLink {
                source: back.source,
                site: back.site,
                by_id: back.by_id,
            });
        }
        Ok(out)
    }

    /// **Demote** an attachment card to a row: fold the sidecar `card` — or
    /// the sidecar of the payload `card` names — into a row of the document
    /// `into` (default: the card's parent), in the `type: payload` field
    /// `field` names (or the only one declared), and delete the sidecar. The
    /// payload stays where it is; `into` now owns it.
    ///
    /// The row holds the link to the payload and every key of the sidecar
    /// but the node's own: its `id`, its link to its parent, its `content`
    /// pointer (which becomes the row's value) and its `attachment` marker.
    /// A `title` is kept unless it is the one `attach` would have derived
    /// from the file's name, so a promotion gives the card back the title it
    /// had. A `content_hash` is copied as written; where the workspace records
    /// checksums and the card had none, one is computed.
    ///
    /// One change set: the sidecar goes, the parent's entry for it goes, the
    /// row is written, and the card's id is retired. Refuses a card anything
    /// links to by path or id (each named — the link would dangle; a link to
    /// the *payload* is fine, since the payload stays), a card with children
    /// or any relation besides its parent link (a row carries neither), a
    /// shadowed document (`attach --opaque`; a row owns only opaque files),
    /// and a payload `into` already owns. Returns the document the row went
    /// into and the new value's address.
    pub async fn demote(
        &mut self,
        card: &Path,
        into: Option<&Path>,
        field: Option<&str>,
    ) -> Result<(PathBuf, Address)> {
        let mut card = link::normalize(card);
        let payload = match self.attachment_payload(&card).await {
            Some(payload) => payload,
            None => match self.attachment_for(&card).await? {
                Some(sidecar) => {
                    let payload = card;
                    card = sidecar;
                    payload
                }
                None => {
                    return Err(Error::Structure(format!(
                        "{} is not an attachment card or a file that has one",
                        card.display()
                    )));
                }
            },
        };
        let path = self.row_field(field)?;
        let Some((list, key)) = path.row() else {
            unreachable!("row_field only returns paths of rows");
        };
        if !prov_graph::document::is_opaque_payload(&payload) {
            return Err(Error::Structure(format!(
                "{} shadows a document prov can read; a row owns only opaque files",
                payload.display()
            )));
        }

        let (spanning, inverse) = self.spanning_pair()?;
        let (card_text, card_doc) = self.load(&card).await?;
        let card_meta = fig::Value::from(&card_doc.meta);
        let parent = self.single_target(&card_doc, &inverse, &card);
        let into = match (into, &parent) {
            (Some(into), _) => link::normalize(into),
            (None, Some(parent)) => parent.clone(),
            (None, None) => {
                return Err(Error::Structure(format!(
                    "{} has no parent to fold it into; name a document",
                    card.display()
                )));
            }
        };
        if into == card || into == payload {
            return Err(Error::Structure(format!(
                "{} cannot own its own attachment",
                into.display()
            )));
        }

        let children = self.relations().children(&card_meta);
        if !children.is_empty() {
            return Err(Error::Structure(format!(
                "{} contains {} document(s) ({}); a row cannot",
                card.display(),
                children.len(),
                children.join(", ")
            )));
        }
        let mapping = card_doc.meta.as_mapping().cloned().unwrap_or_default();
        if let Some(relation) = self
            .relations()
            .relations()
            .iter()
            .map(|r| r.name.as_str())
            .find(|name| *name != inverse && mapping.contains_key(*name))
        {
            return Err(Error::Structure(format!(
                "{} holds `{relation}`, a relation a row cannot carry; it stays a card",
                card.display()
            )));
        }
        let links = self.card_links(&card).await?;
        if !links.is_empty() {
            let named: Vec<String> = links
                .iter()
                .map(|l| {
                    format!(
                        "{} ({}{})",
                        l.source.display(),
                        l.site,
                        if l.by_id { ", by id" } else { "" }
                    )
                })
                .collect();
            return Err(Error::Structure(format!(
                "{} is linked to as a card by {}; those links would dangle — \
                 point them at {} instead, or keep the card",
                card.display(),
                named.join(", "),
                payload.display()
            )));
        }

        let (into_text, into_doc) = self.load(&into).await?;
        let into_meta = fig::Value::from(&into_doc.meta);
        if owned_payloads(self.payloads(), &into, &into_meta)
            .iter()
            .any(|owned| owned.target == payload)
        {
            return Err(Error::Structure(format!(
                "{} already owns {}",
                into.display(),
                payload.display()
            )));
        }

        // The row: the value first, then the card's facts in the card's order.
        let value_address = list.to_string();
        let site = LinkSite::Field {
            path: value_address.clone(),
        };
        let spelled = link::path_text(self.site_path_style(&site), &into, &payload);
        let derived_title = link::path_to_title(&payload);
        let mut row = vec![(fig::Value::Str(key.to_string()), fig::Value::Str(spelled))];
        for (k, v) in &mapping {
            let node_key = NODE_KEYS.contains(&k.as_str()) || *k == inverse || k == "content";
            let derived = k == "title"
                && prov_graph::title::title_text(v).as_deref() == Some(derived_title.as_str());
            if node_key || derived || k == key {
                continue;
            }
            row.push((fig::Value::Str(k.clone()), fig::Value::from(v)));
        }
        if self.fixity().is_on() && !mapping.contains_key(HASH_KEY) {
            row.push((
                fig::Value::Str(HASH_KEY.to_string()),
                fig::Value::Str(self.file_digest(&payload).await?),
            ));
        }
        let row = fig::Value::Map(row);

        // The parent loses its entry for the card — in the same rendering as
        // the row when the parent is the document the row goes into.
        let entry = |ws: &Self, doc: &prov_graph::document::Document, at: &Path| {
            ws.entry_index(doc, &spanning, at, &card)
        };
        // Its last entry takes the list with it, rather than leaving `[]`.
        let unlist = |editor: &mut MetaEditor,
                      doc: &prov_graph::document::Document,
                      index: usize|
         -> Result<()> {
            let entries = doc
                .meta
                .get(&spanning)
                .map_or(0, |v| v.link_strings().len());
            remove_entry(editor, &[fig::Segment::Key(&spanning)], index, entries)
        };
        let mut into_editor = MetaEditor::open_or_init(&into_text, into_doc.carrier)?;
        if parent.as_ref() == Some(&into)
            && let Some(index) = entry(self, &into_doc, &into)
        {
            unlist(&mut into_editor, &into_doc, index)?;
        }
        let existing = field::values_at(&into_meta, &FieldPath::parse(&value_address))
            .into_iter()
            .next()
            .map(|(_, v)| v.items().map_or(0, <[fig::Value]>::len));
        let segments = list.segments();
        let index = match existing {
            Some(len) => {
                into_editor.append_value(&segments, row)?;
                len
            }
            None => {
                into_editor.set_value(&segments, fig::Value::Seq(vec![row]))?;
                0
            }
        };
        let into_out = into_editor.render()?;

        let mut parent_write = None;
        if let Some(parent) = parent.as_ref().filter(|p| **p != into) {
            let (parent_text, parent_doc) = self.load(parent).await?;
            if let (Some(index), Some(carrier)) =
                (entry(self, &parent_doc, parent), parent_doc.carrier)
            {
                let mut editor = MetaEditor::open(&parent_text, carrier)?;
                unlist(&mut editor, &parent_doc, index)?;
                parent_write = Some((parent.clone(), parent_text, editor.render()?));
            }
        }

        let id = self.index().id_for_path(&card);
        let mut cs = self.change();
        cs.expect(&card, card_text);
        cs.remove(&card);
        cs.expect(&into, into_text);
        cs.write(&into, into_out);
        if let Some((parent, before, after)) = parent_write {
            cs.expect(&parent, before);
            cs.write(&parent, after);
        }
        if let Some(id) = id {
            self.index_mut().unregister(&id);
        }
        self.commit(cs).await?;
        let at = Address::parse(&format!("{value_address}[{index}].{key}"))
            .expect("a row address has no `[]`");
        Ok((into, at))
    }

    /// The document that owns `payload` through a `type: payload` value, and
    /// the value's address — found by a census from the workspace root.
    /// `None` when nothing owns it; refused when more than one document does
    /// (`PayloadOwnedTwice`), since which row to promote is then a choice.
    pub async fn payload_owner(&self, payload: &Path) -> Result<Option<(PathBuf, Address)>> {
        let payload = link::normalize(payload);
        if self.payloads().is_empty() {
            return Ok(None);
        }
        let Some(root) = self.root_document().await? else {
            return Ok(None);
        };
        let mut owners: Vec<(PathBuf, Address)> = Vec::new();
        for back in self.backlinks_to(&root, &payload).await? {
            if !matches!(back.site, LinkSite::Field { .. })
                || owners.iter().any(|(doc, _)| *doc == back.source)
            {
                continue;
            }
            for owned in self.owned_payloads(&back.source).await? {
                if owned.target == payload {
                    owners.push((back.source.clone(), owned.address));
                }
            }
        }
        match owners.len() {
            0 | 1 => Ok(owners.pop()),
            _ => {
                let named: Vec<String> = owners
                    .iter()
                    .map(|(doc, at)| format!("{} ({at})", doc.display()))
                    .collect();
                Err(Error::Structure(format!(
                    "{} is owned more than once ({}); drop all but one row first",
                    payload.display(),
                    named.join(", ")
                )))
            }
        }
    }

    /// **Promote** an owned payload to a card: mint the sidecar beside
    /// `payload` that [`attach`](Self::attach) would, carrying every other key
    /// of the row that owns it (a `title` among them becomes the card's), list
    /// it under `parent` (default: the document that owned it), and remove
    /// the row. One change set. The payload stays where it is, and every link
    /// to it — the owner's own `![](…)` — still resolves. Returns the
    /// sidecar's path.
    ///
    /// Refuses a payload no document owns, or more than one does, and what
    /// [`attach`](Self::attach) refuses of a payload — one that already has a
    /// sidecar among them.
    pub async fn promote(&mut self, payload: &Path, parent: Option<&Path>) -> Result<PathBuf> {
        let payload = link::normalize(payload);
        let Some((owner, address)) = self.payload_owner(&payload).await? else {
            return Err(Error::Structure(format!(
                "no document owns {} through a `type: payload` field",
                payload.display()
            )));
        };
        self.check_attach(&payload, false).await?;
        let parent = parent.map(link::normalize).unwrap_or_else(|| owner.clone());
        let (spanning, _) = self.spanning_pair()?;

        let Some(row_address) = address.as_item_parent() else {
            return Err(Error::Structure(format!(
                "{} names {} at {address}, which is not a row of a list",
                owner.display(),
                payload.display()
            )));
        };
        let (list, index) = row_address
            .as_item()
            .expect("as_item_parent returns an item address");
        let Some(value_key) = address.last_key() else {
            unreachable!("a value in a row is at a key");
        };

        let (owner_text, owner_doc) = self.load(&owner).await?;
        let owner_meta = fig::Value::from(&owner_doc.meta);
        let row = field::values_at(&owner_meta, &FieldPath::parse(&row_address.to_string()))
            .into_iter()
            .next()
            .map(|(_, v)| v.clone());
        let Some(fig::Value::Map(row)) = row else {
            unreachable!("the owner's value sits in a row");
        };
        let rows_left = field::values_at(&owner_meta, &FieldPath::parse(&list.to_string()))
            .into_iter()
            .next()
            .map_or(0, |(_, v)| v.items().map_or(0, <[fig::Value]>::len))
            .saturating_sub(1);

        let mut title = link::path_to_title(&payload);
        let mut carried = prov_graph::meta::Mapping::new();
        for (k, v) in row {
            let Some(k) = k.as_str().map(str::to_owned) else {
                continue;
            };
            if k == value_key {
                continue;
            }
            let v = prov_graph::meta::Value::from(v);
            if k == "title" {
                if let Some(text) = prov_graph::title::title_text(&v) {
                    title = text;
                }
                continue;
            }
            carried.insert(k, v);
        }

        let (parent_text, parent_doc) = if parent == owner {
            (owner_text.clone(), owner_doc.clone())
        } else {
            self.load(&parent).await?
        };
        let mut cs = self.change();
        let staged = self
            .stage_sidecar(&payload, &parent, &parent_doc, title, carried)
            .await?;

        // The owner loses the row (and the list, with its last row); the
        // parent gains the card — in one rendering when they are one file.
        let mut owner_editor = MetaEditor::open_or_init(&owner_text, owner_doc.carrier)?;
        remove_entry(&mut owner_editor, &list.segments(), index, rows_left + 1)?;
        let span_path = [fig::Segment::Key(&spanning)];
        let append = |editor: &mut MetaEditor| -> Result<()> {
            if editor
                .append_value(&span_path, fig::Value::Str(staged.down.clone()))
                .is_err()
            {
                editor.set_value(
                    &span_path,
                    fig::Value::Seq(vec![fig::Value::Str(staged.down.clone())]),
                )?;
            }
            Ok(())
        };
        cs.expect(&owner, owner_text);
        if parent == owner {
            append(&mut owner_editor)?;
        } else {
            let mut parent_editor = MetaEditor::open_or_init(&parent_text, parent_doc.carrier)?;
            append(&mut parent_editor)?;
            cs.expect(&parent, parent_text);
            cs.write(&parent, parent_editor.render()?);
        }
        cs.write(&owner, owner_editor.render()?);
        cs.expect_absent(&staged.node);
        cs.write(&staged.node, staged.text);
        self.commit(cs).await?;
        Ok(staged.node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_testkit::{read, write};

    fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("inline", tag)
    }

    fn ws(dir: &Path) -> Workspace<StdFs> {
        Workspace::builder(StdFs)
            .root(dir)
            .payloads(vec![FieldPath::parse("attachments[].content")])
            .build()
    }

    #[test]
    fn a_row_is_read_with_its_target_and_its_checksum() {
        let meta = fig::Value::from(&prov_graph::meta::Value::Mapping(
            prov_graph::meta::parse_mapping(
                "attachments:\n- content: fjord.jpg\n  content_hash: sha256:ab\n  people: [Ada]\n- content: https://example.org/x.jpg\n- content: id:abc\n",
                prov_graph::MetaFormat::Yaml,
            )
            .unwrap(),
        ));
        let owned = owned_payloads(
            &[FieldPath::parse("attachments[].content")],
            Path::new("trips/trip.md"),
            &meta,
        );
        assert_eq!(owned.len(), 1, "a URL and an id own nothing: {owned:?}");
        assert_eq!(owned[0].target, PathBuf::from("trips/fjord.jpg"));
        assert_eq!(owned[0].address.to_string(), "attachments[0].content");
        let (at, recorded) = owned[0].hash.clone().unwrap();
        assert_eq!(at.to_string(), "attachments[0].content_hash");
        assert_eq!(recorded, "sha256:ab");
    }

    #[test]
    fn attach_inline_appends_a_row_with_a_checksum() {
        let dir = tempdir("append");
        write(
            &dir,
            "index.md",
            b"---\ntitle: Home\ncontents:\n- trip.md\n---\n",
        );
        write(
            &dir,
            "trip.md",
            b"---\ntitle: Trip\npart_of: index.md\n---\nWe sailed.\n",
        );
        write(&dir, "fjord.jpg", [0xff, 0xd8, 0xff]);
        write(&dir, "map.png", [0x89, 0x50]);

        let mut w = ws(&dir);
        let at =
            block_on(w.attach_inline(Path::new("fjord.jpg"), Path::new("trip.md"), None)).unwrap();
        assert_eq!(at.to_string(), "attachments[0].content");
        let at =
            block_on(w.attach_inline(Path::new("map.png"), Path::new("trip.md"), None)).unwrap();
        assert_eq!(at.to_string(), "attachments[1].content");

        let text = read(&dir, "trip.md");
        let digest = crate::fixity::digest(&[0xff, 0xd8, 0xff]);
        // Spelled in the workspace's link style, root-relative by default.
        assert!(text.contains("content: /fjord.jpg"), "{text}");
        assert!(text.contains(&format!("content_hash: {digest}")), "{text}");
        assert!(
            text.ends_with("We sailed.\n"),
            "the body is untouched: {text}"
        );
        assert!(!dir.join("fjord.jpg.yaml").exists(), "no sidecar");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        let again = block_on(w.attach_inline(Path::new("fjord.jpg"), Path::new("trip.md"), None));
        assert!(
            again.unwrap_err().to_string().contains("already owns"),
            "a payload is owned once"
        );
    }

    /// A home, a trip in `days/` that owns `days/fjord.jpg` and shows it, and
    /// a second page elsewhere that links the picture too.
    fn trip(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            b"---\ntitle: Home\ncontents:\n- days/trip.md\n- other.md\n---\n",
        );
        write(
            &dir,
            "days/trip.md",
            b"---\ntitle: Trip\npart_of: ../index.md\nattachments:\n- content: fjord.jpg\n  people: [Ada]\n---\n![](fjord.jpg)\n",
        );
        write(&dir, "days/fjord.jpg", [0xff, 0xd8]);
        write(
            &dir,
            "other.md",
            b"---\ntitle: Other\npart_of: index.md\n---\nSee ![](days/fjord.jpg).\n",
        );
        dir
    }

    #[test]
    fn a_move_to_another_directory_takes_the_payload_along() {
        let dir = trip("move");
        let mut w = ws(&dir);
        block_on(w.rename(Path::new("days/trip.md"), Path::new("trips/2026/trip.md"))).unwrap();

        assert!(!dir.join("days/fjord.jpg").exists(), "the payload left");
        assert!(
            dir.join("trips/2026/fjord.jpg").exists(),
            "and landed beside it"
        );
        let text = read(&dir, "trips/2026/trip.md");
        // Respelled in the workspace's link style, root-relative by default.
        assert!(text.contains("- content: /trips/2026/fjord.jpg"), "{text}");
        assert!(
            text.contains("people: [Ada]"),
            "the row's facts are kept: {text}"
        );
        assert!(text.contains("![](/trips/2026/fjord.jpg)"), "{text}");
        let other = read(&dir, "other.md");
        assert!(
            other.contains("/trips/2026/fjord.jpg"),
            "an inbound link follows: {other}"
        );
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_rename_in_place_moves_nothing_but_the_document() {
        let dir = trip("in-place");
        let mut w = ws(&dir);
        block_on(w.rename(Path::new("days/trip.md"), Path::new("days/voyage.md"))).unwrap();
        assert!(dir.join("days/fjord.jpg").exists());
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_payload_outside_the_documents_directory_stays_and_is_respelled() {
        let dir = trip("outside");
        write(&dir, "shared/map.png", [0x89]);
        let text = read(&dir, "days/trip.md").replace(
            "  people: [Ada]\n",
            "  people: [Ada]\n- content: ../shared/map.png\n",
        );
        write(&dir, "days/trip.md", text.as_bytes());

        let mut w = ws(&dir);
        block_on(w.rename(Path::new("days/trip.md"), Path::new("trips/2026/trip.md"))).unwrap();
        assert!(
            dir.join("shared/map.png").exists(),
            "not the owner's to move"
        );
        let text = read(&dir, "trips/2026/trip.md");
        assert!(text.contains("content: /shared/map.png"), "{text}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn a_move_onto_an_existing_file_is_refused_and_changes_nothing() {
        let dir = trip("occupied");
        write(&dir, "trips/fjord.jpg", [0x00]);
        let before = read(&dir, "days/trip.md");
        let mut w = ws(&dir);
        let err =
            block_on(w.rename(Path::new("days/trip.md"), Path::new("trips/trip.md"))).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(read(&dir, "days/trip.md"), before);
        assert!(dir.join("days/fjord.jpg").exists());
    }

    #[test]
    fn a_delete_takes_the_payload_and_reports_who_still_showed_it() {
        let dir = trip("delete");
        let mut w = ws(&dir);
        let danglers = block_on(w.delete(Path::new("days/trip.md"), false)).unwrap();

        assert!(
            !dir.join("days/fjord.jpg").exists(),
            "the payload went with it"
        );
        assert!(
            danglers.iter().any(|f| matches!(
                f,
                crate::validate::Finding::BrokenLink { doc, .. } if doc == Path::new("other.md")
            )),
            "the page that showed it is told: {danglers:?}"
        );
        let log = read(&dir, "deletions/index.yaml");
        assert!(log.contains("from: days/trip.md"), "{log}");
        assert!(log.contains("payloads:"), "{log}");
        assert!(log.contains("days/fjord.jpg"), "{log}");
    }

    #[test]
    fn a_delete_leaves_a_readable_document_a_row_names() {
        let dir = trip("delete-document");
        write(&dir, "days/notes.md", b"---\ntitle: Notes\n---\n");
        let text = read(&dir, "days/trip.md").replace(
            "  people: [Ada]\n",
            "  people: [Ada]\n- content: notes.md\n",
        );
        write(&dir, "days/trip.md", text.as_bytes());

        block_on(ws(&dir).delete(Path::new("days/trip.md"), false)).unwrap();
        assert!(
            dir.join("days/notes.md").exists(),
            "a document is never a payload to delete"
        );
    }

    #[test]
    fn check_catches_a_changed_payload_and_a_restamp_accepts_it() {
        let dir = trip("fixity");
        let mut w = ws(&dir);
        assert_eq!(
            block_on(w.restamp_payloads(Path::new("days/trip.md")))
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["attachments[0].content_hash"],
            "a row with no checksum earns one"
        );
        assert!(
            block_on(w.restamp_payloads(Path::new("days/trip.md")))
                .unwrap()
                .is_empty(),
            "unchanged bytes are owed nothing"
        );
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        write(&dir, "days/fjord.jpg", [0x00]);
        let findings = block_on(w.check("index.md")).unwrap();
        let [finding] = findings.as_slice() else {
            panic!("{findings:?}");
        };
        let crate::validate::Finding::PayloadFixityMismatch {
            doc,
            address,
            payload,
            ..
        } = finding
        else {
            panic!("{finding:?}");
        };
        assert_eq!(doc, Path::new("days/trip.md"));
        assert_eq!(address, "attachments[0].content_hash");
        assert_eq!(payload, Path::new("days/fjord.jpg"));

        let remedies = block_on(w.remedies(finding)).unwrap();
        block_on(w.apply_fix(&remedies[0].fix)).unwrap();
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
        assert!(
            read(&dir, "days/trip.md").contains("people: [Ada]"),
            "the row's facts are kept"
        );
    }

    #[test]
    fn check_reports_a_file_with_two_owners() {
        let dir = trip("owned-twice");
        let text = read(&dir, "other.md").replace(
            "part_of: index.md\n",
            "part_of: index.md\nattachments:\n- content: days/fjord.jpg\n",
        );
        write(&dir, "other.md", text.as_bytes());
        let findings = block_on(ws(&dir).check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(
                f,
                crate::validate::Finding::PayloadOwnedTwice { payload, .. }
                    if payload == Path::new("days/fjord.jpg")
            )),
            "{findings:?}"
        );
        assert_eq!(findings.len(), 1, "{findings:?}");
    }

    #[test]
    fn a_file_with_a_sidecar_of_its_own_is_the_cards_and_stays() {
        let dir = trip("carded");
        write(
            &dir,
            "days/fjord.jpg.yaml",
            b"title: Fjord\npart_of: ../index.md\ncontent: fjord.jpg\nattachment: true\n",
        );
        let mut w = ws(&dir);
        block_on(w.rename(Path::new("days/trip.md"), Path::new("trips/trip.md"))).unwrap();
        assert!(
            dir.join("days/fjord.jpg").exists(),
            "the card keeps its payload"
        );
        block_on(w.delete(Path::new("trips/trip.md"), false)).unwrap();
        assert!(
            dir.join("days/fjord.jpg").exists(),
            "and a delete leaves it too"
        );
    }

    #[test]
    fn attach_inline_refuses_a_document_and_an_undeclared_field() {
        let dir = tempdir("refuse");
        write(&dir, "index.md", b"---\ntitle: Home\n---\n");
        write(&dir, "note.md", b"---\ntitle: Note\n---\n");
        write(&dir, "fjord.jpg", [0xff]);

        let mut w = ws(&dir);
        let err = block_on(w.attach_inline(Path::new("note.md"), Path::new("index.md"), None))
            .unwrap_err();
        assert!(
            err.to_string().contains("not an opaque attachment"),
            "{err}"
        );

        let err = block_on(w.attach_inline(
            Path::new("fjord.jpg"),
            Path::new("index.md"),
            Some("media[].src"),
        ))
        .unwrap_err();
        assert!(err.to_string().contains("not declared"), "{err}");

        let mut bare = Workspace::builder(StdFs).root(&dir).build();
        let err = block_on(bare.attach_inline(Path::new("fjord.jpg"), Path::new("index.md"), None))
            .unwrap_err();
        assert!(err.to_string().contains("no field is declared"), "{err}");
    }
    /// A home and a devlog whose two pictures are cards under it, shown in
    /// its body — the shape a devlog took before rows existed.
    fn carded(tag: &str) -> PathBuf {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            b"---\ntitle: Home\ncontents:\n- log/log.md\n---\n",
        );
        write(
            &dir,
            "log/log.md",
            b"---\ntitle: Log\npart_of: ../index.md\ncontents:\n- rules.svg.yaml\n- speed.svg.yaml\n---\n![](rules.svg)\n",
        );
        write(&dir, "log/rules.svg", b"<svg/>");
        write(&dir, "log/speed.svg", b"<svg></svg>");
        let digest = crate::fixity::digest(b"<svg/>");
        write(
            &dir,
            "log/rules.svg.yaml",
            format!(
                "title: Rules\npart_of: log.md\ncontent: rules.svg\nattachment: true\ncontent_hash: {digest}\n"
            )
            .as_bytes(),
        );
        write(
            &dir,
            "log/speed.svg.yaml",
            b"title: How fast it is\npart_of: log.md\ncontent: speed.svg\nattachment: true\nalt: a chart\n",
        );
        dir
    }

    #[test]
    fn demote_folds_a_card_into_a_row_on_its_parent() {
        let dir = carded("demote");
        let mut w = ws(&dir);
        let (into, at) = block_on(w.demote(Path::new("log/rules.svg.yaml"), None, None)).unwrap();
        assert_eq!(into, PathBuf::from("log/log.md"));
        assert_eq!(at.to_string(), "attachments[0].content");
        // Named by its payload, too.
        let (_, at) = block_on(w.demote(Path::new("log/speed.svg"), None, None)).unwrap();
        assert_eq!(at.to_string(), "attachments[1].content");

        assert!(!dir.join("log/rules.svg.yaml").exists(), "the card is gone");
        assert!(!dir.join("log/speed.svg.yaml").exists());
        assert!(dir.join("log/rules.svg").exists(), "the payload stays");
        let text = read(&dir, "log/log.md");
        assert!(!text.contains("contents"), "{text}");
        assert!(text.contains("- content: /log/rules.svg\n"), "{text}");
        assert!(
            !text.contains("title: Rules"),
            "a title derived from the name is dropped: {text}"
        );
        assert!(
            text.contains("title: How fast it is"),
            "an authored one is kept: {text}"
        );
        assert!(text.contains("alt: a chart"), "{text}");
        assert!(
            text.contains(&format!(
                "content_hash: {}",
                crate::fixity::digest(b"<svg/>")
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "content_hash: {}",
                crate::fixity::digest(b"<svg></svg>")
            )),
            "a card with no checksum earns one: {text}"
        );
        assert!(!text.contains("attachment: true"), "{text}");
        assert!(text.ends_with("![](rules.svg)\n"), "{text}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn demote_into_another_document_edits_both() {
        let dir = carded("demote-into");
        write(
            &dir,
            "index.md",
            b"---\ntitle: Home\ncontents:\n- log/log.md\n- other.md\n---\n",
        );
        write(
            &dir,
            "other.md",
            b"---\ntitle: Other\npart_of: index.md\n---\n",
        );
        let mut w = ws(&dir);
        block_on(w.demote(
            Path::new("log/speed.svg.yaml"),
            Some(Path::new("other.md")),
            None,
        ))
        .unwrap();
        let log = read(&dir, "log/log.md");
        assert!(!log.contains("speed.svg.yaml"), "{log}");
        assert!(log.contains("rules.svg.yaml"), "{log}");
        let other = read(&dir, "other.md");
        assert!(other.contains("- content: /log/speed.svg"), "{other}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn demote_refuses_a_card_something_links_to() {
        let dir = carded("demote-linked");
        write(
            &dir,
            "index.md",
            b"---\ntitle: Home\ncontents:\n- log/log.md\nlinks:\n- log/rules.svg.yaml\n---\n",
        );
        let before = read(&dir, "log/log.md");
        let mut w = ws(&dir);
        let err = block_on(w.demote(Path::new("log/rules.svg.yaml"), None, None)).unwrap_err();
        assert!(err.to_string().contains("index.md (links"), "{err}");
        assert_eq!(read(&dir, "log/log.md"), before);
        assert!(dir.join("log/rules.svg.yaml").exists());
        // A link to the payload is no reason to refuse: the payload stays.
        block_on(w.demote(Path::new("log/speed.svg.yaml"), None, None)).unwrap();
    }

    #[test]
    fn promote_gives_a_row_its_card_back() {
        let dir = carded("promote");
        let mut w = ws(&dir);
        block_on(w.demote(Path::new("log/rules.svg.yaml"), None, None)).unwrap();
        block_on(w.demote(Path::new("log/speed.svg.yaml"), None, None)).unwrap();

        let node = block_on(w.promote(Path::new("log/speed.svg"), None)).unwrap();
        assert_eq!(node, PathBuf::from("log/speed.svg.yaml"));
        let card = read(&dir, "log/speed.svg.yaml");
        assert!(card.contains("title: How fast it is"), "{card}");
        assert!(card.contains("content: speed.svg"), "{card}");
        assert!(card.contains("attachment: true"), "{card}");
        assert!(card.contains("alt: a chart"), "{card}");
        let log = read(&dir, "log/log.md");
        assert!(log.contains("speed.svg.yaml"), "listed: {log}");
        assert!(!log.contains("content: /log/speed.svg"), "row gone: {log}");
        assert!(log.contains("content: /log/rules.svg"), "{log}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        // The last row takes its list with it.
        block_on(w.promote(Path::new("log/rules.svg"), None)).unwrap();
        let log = read(&dir, "log/log.md");
        assert!(!log.contains("attachments"), "{log}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        let err = block_on(w.promote(Path::new("log/rules.svg"), None)).unwrap_err();
        assert!(err.to_string().contains("no document owns"), "{err}");
    }
    /// fig's own devlogs: a figl block whose `contents` is a `>` block list,
    /// which fig's editor will not delete as a key. Demoting the last card
    /// leaves `contents = []` instead, and the row still lands and checks clean.
    #[test]
    fn demote_into_a_figl_block_with_a_block_list() {
        let dir = tempdir("demote-figl");
        write(
            &dir,
            "index.md",
            b"---\ntitle: Home\ncontents:\n- log.md\n---\n",
        );
        write(
            &dir,
            "log.md",
            b"---figl\ntitle = Log\npart_of = index.md\ncontents\n> * [Rules](/rules.svg.yaml)\n---\n![](rules.svg)\n",
        );
        write(&dir, "rules.svg", b"<svg/>");
        write(
            &dir,
            "rules.svg.yaml",
            b"title: Rules\npart_of: log.md\ncontent: rules.svg\nattachment: true\n",
        );
        let mut w = ws(&dir);
        block_on(w.demote(Path::new("rules.svg"), None, None)).unwrap();
        let text = read(&dir, "log.md");
        assert!(!text.contains("rules.svg.yaml"), "{text}");
        // fig 7 writes the spliced row in flow form (`{content=/rules.svg …}`).
        let doc = prov_graph::Document::parse("log.md", &text).unwrap();
        assert_eq!(
            doc.meta
                .get_path("attachments")
                .and_then(|a| a.as_sequence())
                .and_then(|a| a.first())
                .and_then(|row| row.get("content"))
                .and_then(prov_graph::Value::as_str),
            Some("/rules.svg"),
            "{text}"
        );
        assert!(!dir.join("rules.svg.yaml").exists());
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }
}
