//! What kind of record a document is — a page, a picture, a recording, a
//! directory of files — for the frontends and the filing entries that treat
//! them differently.
//!
//! prov already knows the structural difference between a document that is
//! its own record and an [attachment](crate::document::Document::is_attachment)
//! whose record is a sidecar beside some bytes, and between either of those
//! and a [manifest node](crate::document::Document::is_manifest_node) standing
//! for a whole directory. What it did not say is what the bytes *are*, and
//! that is the question a person adding something asks first: a photograph and
//! a scanned deed are both attachments, and a library may well file them in
//! different places. So the kind of a payload is read here, once, from its
//! extension — the same evidence [`is_opaque_payload`] decides opacity from,
//! for the same reason: it is answerable before a byte is read, and a wrong
//! answer is visible and correctable, where sniffing would mean opening a
//! video to name it.
//!
//! The payload kinds follow the IANA top-level media types (`image`, `audio`,
//! `video`), with `file` for every other opaque payload, so the words mean what
//! they mean in a `Content-Type`.

use std::path::Path;

use crate::document::{Document, is_opaque_payload};

/// What kind of record a document is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RecordKind {
    /// A document whose content prov reads: prose, or a metadata document
    /// that is its own record.
    Page,
    /// An attachment whose payload is a picture.
    Image,
    /// An attachment whose payload is sound.
    Audio,
    /// An attachment whose payload is moving pictures.
    Video,
    /// An attachment whose payload is anything else — a PDF, an archive, a
    /// font.
    File,
    /// A manifest node, standing for a directory of files.
    Manifest,
}

/// The spelling of each kind, in [`RecordKind`]'s order — what a `kind:`
/// value names.
pub const RECORD_KINDS: &[&str] = &["page", "image", "audio", "video", "file", "manifest"];

/// The word that names every payload kind at once — `image`, `audio`,
/// `video` and `file` — in a `kind:` list.
pub const ATTACHMENT_KINDS: &str = "attachment";

impl RecordKind {
    /// The spelling a `kind:` value uses.
    pub fn as_str(self) -> &'static str {
        match self {
            RecordKind::Page => "page",
            RecordKind::Image => "image",
            RecordKind::Audio => "audio",
            RecordKind::Video => "video",
            RecordKind::File => "file",
            RecordKind::Manifest => "manifest",
        }
    }

    /// Read one spelling, trimmed and case-insensitive. `None` for anything
    /// else, [`ATTACHMENT_KINDS`] included — that word names several kinds,
    /// and a caller expanding a list asks [`parse_list_item`](Self::parse_list_item).
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "page" => Some(RecordKind::Page),
            "image" => Some(RecordKind::Image),
            "audio" => Some(RecordKind::Audio),
            "video" => Some(RecordKind::Video),
            "file" => Some(RecordKind::File),
            "manifest" => Some(RecordKind::Manifest),
            _ => None,
        }
    }

    /// The kinds one item of a `kind:` list names: a single kind, or every
    /// payload kind for [`ATTACHMENT_KINDS`]. `None` for an unknown word.
    pub fn parse_list_item(text: &str) -> Option<Vec<Self>> {
        if text.trim().eq_ignore_ascii_case(ATTACHMENT_KINDS) {
            return Some(Self::payload_kinds().to_vec());
        }
        Self::parse(text).map(|kind| vec![kind])
    }

    /// The kinds an attachment can be, in order.
    pub fn payload_kinds() -> &'static [RecordKind] {
        &[
            RecordKind::Image,
            RecordKind::Audio,
            RecordKind::Video,
            RecordKind::File,
        ]
    }

    /// Whether this is the kind of an attachment's payload.
    pub fn is_payload(self) -> bool {
        Self::payload_kinds().contains(&self)
    }
}

/// The kind of the opaque payload at `path`, from its extension — `Image`,
/// `Audio`, `Video`, or `File` for anything else. A path prov can read
/// ([`is_opaque_payload`] is false) is a [`Page`](RecordKind::Page): it is a
/// document, not a payload, whatever it is called.
pub fn payload_kind(path: &Path) -> RecordKind {
    if !is_opaque_payload(path) {
        return RecordKind::Page;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "bmp" | "tif" | "tiff"
        | "svg" | "avif" | "jxl" | "dng" | "raw" => RecordKind::Image,
        "mp3" | "m4a" | "aac" | "wav" | "aiff" | "aif" | "flac" | "ogg" | "oga" | "opus"
        | "caf" | "amr" => RecordKind::Audio,
        "mp4" | "mov" | "m4v" | "webm" | "avi" | "mkv" | "mpg" | "mpeg" | "3gp" => {
            RecordKind::Video
        }
        _ => RecordKind::File,
    }
}

impl Document {
    /// What kind of record this document is: a manifest node, an attachment
    /// of the kind its payload is, or a page. `path` is the document's own
    /// workspace path, which a `content` pointer is relative to.
    ///
    /// A node declaring both `content` and `manifest` is the
    /// [conflict](Document::manifest_conflicts) `check` reports; it reads as
    /// a manifest here, since that is the claim covering more files.
    pub fn record_kind(&self, path: &Path) -> RecordKind {
        if self.is_manifest_node() {
            return RecordKind::Manifest;
        }
        if self.is_attachment()
            && let Some(payload) = self.content_path(path)
        {
            let kind = payload_kind(&payload);
            // An `attachment: true` flag over a readable payload is a
            // shadowed document (`attach --opaque`): bytes on purpose, and
            // of no media kind.
            return if kind == RecordKind::Page {
                RecordKind::File
            } else {
                kind
            };
        }
        RecordKind::Page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_is_named_by_its_extension() {
        assert_eq!(
            payload_kind(Path::new("a/IMG_0001.HEIC")),
            RecordKind::Image
        );
        assert_eq!(payload_kind(Path::new("memo.m4a")), RecordKind::Audio);
        assert_eq!(payload_kind(Path::new("clip.mov")), RecordKind::Video);
        assert_eq!(payload_kind(Path::new("deed.pdf")), RecordKind::File);
        assert_eq!(payload_kind(Path::new("noextension")), RecordKind::File);
        assert_eq!(payload_kind(Path::new("note.md")), RecordKind::Page);
    }

    #[test]
    fn every_spelling_round_trips_and_attachment_names_the_payload_kinds() {
        for word in RECORD_KINDS {
            assert_eq!(RecordKind::parse(word).map(RecordKind::as_str), Some(*word));
        }
        assert_eq!(RecordKind::parse(" Image "), Some(RecordKind::Image));
        assert_eq!(RecordKind::parse("attachment"), None);
        assert_eq!(
            RecordKind::parse_list_item("attachment").as_deref(),
            Some(RecordKind::payload_kinds())
        );
        assert_eq!(RecordKind::parse_list_item("photo"), None);
    }

    #[cfg(feature = "yaml")]
    #[test]
    fn a_document_reads_as_the_record_it_is() {
        let page = Document::parse("a.md", "---\ntitle: A\n---\nhi\n").unwrap();
        assert_eq!(page.record_kind(Path::new("a.md")), RecordKind::Page);

        let card = Document::parse("p/a.jpg.yaml", "title: A\ncontent: a.jpg\n").unwrap();
        assert_eq!(
            card.record_kind(Path::new("p/a.jpg.yaml")),
            RecordKind::Image
        );

        let node = Document::parse(
            "photos.yaml",
            "title: Photos\nmanifest: photos.manifest.yaml\n",
        )
        .unwrap();
        assert_eq!(
            node.record_kind(Path::new("photos.yaml")),
            RecordKind::Manifest
        );

        let shadowed = Document::parse(
            "spec.md.yaml",
            "title: Spec\ncontent: spec.md\nattachment: true\n",
        )
        .unwrap();
        assert_eq!(
            shadowed.record_kind(Path::new("spec.md.yaml")),
            RecordKind::File
        );
    }
}
