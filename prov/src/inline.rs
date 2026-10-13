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
            let Ok(bytes) = self.read_bytes(&owned.target).await else {
                continue;
            };
            let digest = crate::fixity::digest(&bytes);
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
            let bytes = self.read_bytes(&payload).await?;
            row.push((
                fig::Value::Str(HASH_KEY.to_string()),
                fig::Value::Str(crate::fixity::digest(&bytes)),
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

#[cfg(all(test, feature = "yaml"))]
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
                fig::Format::Yaml,
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
}
