//! `save`/`record_content_update` — the bookkeeping an edit implies, landed in
//! the same write as the edit itself (or, when the caller already wrote the
//! bytes some other way, reconciled afterward in one more).
//!
//! Neither verb moves, creates, or removes a document — the reason this file
//! exists apart from `create`, `rename`, and the rest — but both are still
//! mutations in the sense this module cares about: they stage a
//! [`ChangeSet`](crate::change::ChangeSet) and commit it, so a failure mid-write
//! cannot leave a document's frontmatter half-stamped.

use std::path::Path;

use crate::workspace::Workspace;
use prov_graph::error::{Error, Result};
use prov_graph::link;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

/// What a document's recorded content checksum says about its bytes right now.
///
/// The three answers are not a scale, they are three different situations, and
/// a caller deciding whether to stamp a timestamp has to tell them apart:
/// [`Drifted`](Self::Drifted) is evidence an edit happened,
/// [`Intact`](Self::Intact) is evidence one did not, and the other two are the
/// absence of evidence rather than either verdict — which is why they are not
/// folded into one of the first two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentState {
    /// The document records no `content_hash` — fixity is off for its kind, or
    /// it predates fixity. Nothing has drifted *from* nothing; a caller that
    /// wants to stamp anyway is making a claim of its own, not restating one.
    Unrecorded,
    /// A recorded digest spelled in an algorithm this build cannot compute (a
    /// newer prov wrote it). Left alone rather than compared or overwritten.
    Unverifiable,
    /// The recorded digest still describes the bytes.
    Intact,
    /// The bytes no longer hash to what the document records. Whether that is
    /// an intended out-of-band edit or corruption is not this answer — see
    /// [`Finding::FixityMismatch`](crate::Finding::FixityMismatch), which
    /// surfaces the same fact as a question.
    Drifted,
}

/// The `content_hash` a document records, if any.
fn recorded_hash(doc: &prov_graph::document::Document) -> Option<&str> {
    doc.meta
        .get("content_hash")
        .and_then(prov_graph::meta::Value::as_str)
}

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Record that a document's content just changed — the single seam for the
    /// bookkeeping an edit implies, done as one crash-safe write.
    ///
    /// Two independent effects, each self-gating:
    /// - **Fixity**: (re)stamp `content_hash` when the workspace records fixity
    ///   for this document's kind (a payload for an attachment, a body otherwise)
    ///   *and* the bytes have actually drifted from what is recorded — so an
    ///   unchanged document restamps nothing.
    /// - **Timestamp**: when `updated` is `Some((field, at))`, set that frontmatter
    ///   `field` to `at`. The *caller* decides an edit happened and supplies the
    ///   time — the library stays clockless and deterministic (DESIGN §2: the
    ///   client produces the instant, prov owns the field and its RFC 3339
    ///   convention). Pass `None` to reconcile the checksum only.
    ///
    /// Returns whether anything was written. Hashes the same bytes `check`
    /// verifies: the `content` sibling for a document that points at one, else the
    /// document's own body.
    pub async fn record_content_update(
        &mut self,
        path: impl AsRef<Path>,
        updated: Option<(&str, &str)>,
    ) -> Result<bool> {
        let path = link::normalize(path.as_ref());
        let (original, doc) = self.load(&path).await?;
        let Some(text) = self.stamped(&path, &original, &doc, updated).await? else {
            return Ok(false);
        };
        let mut cs = self.change();
        cs.write(&path, text);
        self.commit(cs).await?;
        Ok(true)
    }

    /// Write `text` to the document at `path`, stamping what the write itself
    /// implies — the counterpart to
    /// [`record_content_update`](Self::record_content_update) for a caller who
    /// *has* the new text rather than one reconciling text already on disk.
    ///
    /// Same two stamps, decided the same way and documented there: `content_hash`
    /// where the workspace records fixity for this document's kind and the bytes
    /// have drifted, and the `updated` frontmatter field when a caller supplies
    /// one. The difference is only when they are applied. Stamping text on its way
    /// to the disk costs one journaled write; stamping it afterwards costs the
    /// first write, a read back, and a second write of the same document —
    /// three atomic-write protocols where one will do, and, on a synced
    /// filesystem, two uploads of one document per save.
    ///
    /// It is sound to stamp first because neither stamp can invalidate the other
    /// or itself: both live in the frontmatter, and the hash covers the body (or a
    /// `content` sibling), so amending the frontmatter cannot change what the hash
    /// is *of*. The hash the caller's text arrived carrying is what drift is
    /// measured against, exactly as if the text had been read back.
    pub async fn save_document(
        &mut self,
        path: impl AsRef<Path>,
        text: &str,
        updated: Option<(&str, &str)>,
    ) -> Result<()> {
        let path = link::normalize(path.as_ref());
        // The same clamp `load` applies on the way in, owed here too: `path` may
        // have come from a document's own metadata, and this call reaches the
        // filesystem without `load` in front of it to refuse an escape.
        if link::escapes_root(&path) {
            return Err(prov_graph::error::Error::Escape(path));
        }
        let doc = prov_graph::document::Document::parse(&path, text)?;
        let stamped = self.stamped(&path, text, &doc, updated).await?;
        let mut cs = self.change();
        // No stamp applying is not "nothing to do" here, the way it is for
        // `record_content_update`: the caller's text is the point, stamped or not.
        cs.write(&path, stamped.unwrap_or_else(|| text.to_string()));
        self.commit(cs).await
    }

    /// Apply to `text` the frontmatter stamps a content change implies, given the
    /// `doc` that text parses to. `None` when neither applies and `text` already
    /// says what it should.
    ///
    /// The shared middle of [`save_document`](Self::save_document) and
    /// [`record_content_update`](Self::record_content_update). Both make the same
    /// two decisions over the same text; all that differs is whether that text is
    /// on its way to the disk or already there.
    async fn stamped(
        &self,
        path: &Path,
        text: &str,
        doc: &prov_graph::document::Document,
        updated: Option<(&str, &str)>,
    ) -> Result<Option<String>> {
        // Fixity: does this document's kind get hashed, and has it drifted?
        let covered = if doc.is_attachment() {
            self.fixity().covers_payloads()
        } else {
            self.fixity().covers_bodies()
        };
        let new_hash = if covered {
            let hash = self.covered_digest(path, doc).await?;
            (recorded_hash(doc) != Some(hash.as_str())).then_some(hash)
        } else {
            None
        };

        // Apply both frontmatter edits (if any) to the one text, write once.
        let mut text = text.to_string();
        let mut stamped = false;
        if let Some(hash) = new_hash {
            text = prov_store::edit::set_in_text(
                &text,
                doc.carrier,
                "content_hash",
                fig::Value::Str(hash),
            )?;
            stamped = true;
        }
        if let Some((field, at)) = updated
            && !field.is_empty()
        {
            text = prov_store::edit::set_in_text(
                &text,
                doc.carrier,
                field,
                fig::Value::Str(at.to_string()),
            )?;
            stamped = true;
        }
        Ok(stamped.then_some(text))
    }

    /// The digest of the bytes this document's `content_hash` covers — the
    /// `content` sibling when it points at one (an attachment payload, or a
    /// separated prose body), else the document's own body.
    ///
    /// The one place that rule is written down for the write path, so
    /// [`stamped`](Self::stamped) and [`content_state`](Self::content_state)
    /// cannot drift apart on what a hash is *of*.
    ///
    /// Refuses a manifest node outright rather than guessing: its hash covers
    /// the manifest document, not its own (nonexistent) body, and rebuilding
    /// that manifest means re-reading every file it lists — a directory-wide
    /// cost `stamp` was never meant to spend.
    /// [`update_manifest`](crate::workspace::Workspace::update_manifest) —
    /// `prov manifest --update` — is the verb that pays it on purpose.
    async fn covered_digest(
        &self,
        path: &Path,
        doc: &prov_graph::document::Document,
    ) -> Result<String> {
        if doc.is_manifest_node() {
            return Err(Error::Structure(format!(
                "{} is a manifest node — its checksum covers the manifest document \
                 it declares, which `prov manifest {} --update` rebuilds (a \
                 directory-wide rehash); `stamp` does not cover it",
                path.display(),
                path.display(),
            )));
        }
        Ok(match doc.content_attr() {
            Some(raw) => {
                let dir = path.parent().unwrap_or(Path::new(""));
                let target = link::normalize(dir.join(raw));
                crate::fixity::digest(&self.read_bytes(&target).await?)
            }
            None => crate::fixity::digest(doc.body.as_bytes()),
        })
    }

    /// Whether the checksum a document records still describes its bytes —
    /// the question [`record_content_update`](Self::record_content_update)
    /// answers implicitly, asked out loud so a caller can decide *before*
    /// writing.
    ///
    /// It exists because the timestamp half of a stamp has no evidence behind
    /// it: `record_content_update` sets the `updated` field whenever one is
    /// passed, so a caller that cannot say for itself whether an edit happened
    /// (anything that did not own the editor) needs the checksum to tell it.
    /// A sweep over a whole workspace must not bump `updated` on every document
    /// it reads.
    ///
    /// Reads, never writes. Hashes the same bytes
    /// [`check`](Self::check) does, via the same rule
    /// [`record_content_update`](Self::record_content_update) will apply, so a
    /// [`Drifted`](ContentState::Drifted) answer here is exactly a stamp there.
    pub async fn content_state(&self, path: impl AsRef<Path>) -> Result<ContentState> {
        let path = link::normalize(path.as_ref());
        let (_, doc) = self.load(&path).await?;
        let Some(recorded) = recorded_hash(&doc) else {
            return Ok(ContentState::Unrecorded);
        };
        // A digest this build does not know how to compute cannot be compared,
        // and guessing would mean overwriting a future algorithm's hash with
        // this one's. `check` declines the same way, on the same evidence.
        if !crate::fixity::is_recognized(recorded) {
            return Ok(ContentState::Unverifiable);
        }
        let recorded = recorded.to_string();
        Ok(if self.covered_digest(&path, &doc).await? == recorded {
            ContentState::Intact
        } else {
            ContentState::Drifted
        })
    }

    /// Reconcile the content checksum for the document at `path` — [
    /// `record_content_update`](Self::record_content_update) with no timestamp.
    /// The prov-mediated way to keep fixity true across an edit, and how a
    /// document first *earns* a body hash under the `full` tier.
    pub async fn restamp_fixity(&mut self, path: impl AsRef<Path>) -> Result<bool> {
        self.record_content_update(path, None).await
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use crate::validate::Finding;
    use std::path::Path;

    #[test]
    fn full_tier_body_fixity_round_trips_through_restamp_and_check() {
        // The `full` tier: a document's *body* carries its own checksum. The whole
        // prov-edit loop, exercised at the library level (no $EDITOR needed):
        // stamp → verify → out-of-band body edit is caught → restamp re-blesses.
        use crate::config::Fixity;
        let dir = tempdir("fixity-body");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\nhello world\n",
        );

        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Full)
            .build();

        // The document earns a body hash; restamping unchanged bytes is a no-op.
        assert!(
            block_on(w.restamp_fixity("note.md")).unwrap(),
            "first stamp records a hash"
        );
        assert!(
            !block_on(w.restamp_fixity("note.md")).unwrap(),
            "restamp of unchanged bytes writes nothing"
        );
        assert!(
            std::fs::read_to_string(dir.join("note.md"))
                .unwrap()
                .contains("content_hash: sha256:")
        );
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        // Edit the body out-of-band (bypassing `prov edit`) — check catches it.
        let stamped = std::fs::read_to_string(dir.join("note.md")).unwrap();
        std::fs::write(
            dir.join("note.md"),
            stamped.replace("hello world", "goodbye world"),
        )
        .unwrap();
        let findings = block_on(w.check("index.md")).unwrap();
        assert!(
            findings.iter().any(
                |f| matches!(f, Finding::FixityMismatch { doc, .. } if doc == Path::new("note.md"))
            ),
            "an out-of-band body edit must be caught: {findings:?}"
        );

        // Restamp (what `prov edit` does on save) re-blesses it.
        assert!(block_on(w.restamp_fixity("note.md")).unwrap());
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn content_state_tells_the_four_situations_apart() {
        use crate::config::Fixity;
        let dir = tempdir("content-state");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\nbody\n",
        );
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Full)
            .build();

        // No `content_hash` yet: nothing has drifted *from* nothing.
        assert_eq!(
            block_on(w.content_state("note.md")).unwrap(),
            ContentState::Unrecorded
        );

        // Earning one makes the document verifiable, and intact.
        assert!(block_on(w.restamp_fixity("note.md")).unwrap());
        assert_eq!(
            block_on(w.content_state("note.md")).unwrap(),
            ContentState::Intact
        );

        // An out-of-band body edit is drift — the same fact `check` reports as
        // a `FixityMismatch`, which is the agreement `stamp` relies on.
        let text = std::fs::read_to_string(dir.join("note.md")).unwrap();
        std::fs::write(dir.join("note.md"), text.replace("body", "edited")).unwrap();
        assert_eq!(
            block_on(w.content_state("note.md")).unwrap(),
            ContentState::Drifted
        );
        assert!(
            block_on(w.check("index.md"))
                .unwrap()
                .iter()
                .any(|f| matches!(f, crate::Finding::FixityMismatch { .. })),
            "content_state and check must agree about drift"
        );

        // A digest from an algorithm this build cannot compute is left alone
        // rather than compared — the same judgment `check` declines to make.
        let text = std::fs::read_to_string(dir.join("note.md")).unwrap();
        let line = text
            .lines()
            .find(|l| l.starts_with("content_hash:"))
            .unwrap()
            .to_string();
        std::fs::write(
            dir.join("note.md"),
            text.replace(&line, "content_hash: blake9:deadbeef"),
        )
        .unwrap();
        assert_eq!(
            block_on(w.content_state("note.md")).unwrap(),
            ContentState::Unverifiable
        );
    }

    #[test]
    fn content_state_reads_the_payload_for_an_attachment_not_the_sidecar() {
        use crate::config::Fixity;
        let dir = tempdir("content-state-payload");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- photo.jpg.yaml\n---\n",
        );
        std::fs::write(dir.join("photo.jpg"), b"original bytes").unwrap();
        write(
            &dir,
            "photo.jpg.yaml",
            "title: Photo\npart_of: index.md\ncontent: photo.jpg\n",
        );
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Full)
            .build();
        assert!(block_on(w.restamp_fixity("photo.jpg.yaml")).unwrap());
        assert_eq!(
            block_on(w.content_state("photo.jpg.yaml")).unwrap(),
            ContentState::Intact
        );

        // Replacing the payload out of band is what the sidecar's hash covers —
        // the case `stamp <file>` exists for on a binary nothing can diff.
        std::fs::write(dir.join("photo.jpg"), b"replaced bytes").unwrap();
        assert_eq!(
            block_on(w.content_state("photo.jpg.yaml")).unwrap(),
            ContentState::Drifted
        );
    }

    #[test]
    fn a_manifest_node_is_refused_rather_than_hashed_as_a_body() {
        // A manifest node's checksum covers the manifest document it declares,
        // not its own (nonexistent) body — `stamp` cannot restamp it the way it
        // restamps an ordinary document, and must say so rather than silently
        // comparing against the wrong bytes (or, under `full`, overwriting a
        // correct pin with a wrong one).
        let dir = tempdir("content-state-manifest");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        std::fs::create_dir_all(dir.join("photos")).unwrap();
        std::fs::write(dir.join("photos/a.jpg"), b"original").unwrap();

        let mut w = ws(&dir);
        block_on(w.attach_manifest(Path::new("photos"), Path::new("index.md"))).unwrap();

        let err = block_on(w.content_state("photos.yaml")).unwrap_err();
        assert!(
            err.to_string()
                .contains("prov manifest photos.yaml --update"),
            "{err}"
        );

        // The write path agrees, under the tier where it would otherwise reach
        // `covered_digest` at all.
        use crate::config::Fixity;
        let mut full = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Full)
            .build();
        let err = block_on(full.restamp_fixity("photos.yaml")).unwrap_err();
        assert!(err.to_string().contains("manifest"), "{err}");
    }

    #[test]
    fn content_state_never_writes() {
        use crate::config::Fixity;
        let dir = tempdir("content-state-readonly");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\nbody\n",
        );
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Full)
            .build();
        block_on(w.restamp_fixity("note.md")).unwrap();
        let before = std::fs::read_to_string(dir.join("note.md")).unwrap();
        std::fs::write(dir.join("note.md"), before.replace("body", "edited")).unwrap();
        let drifted = std::fs::read_to_string(dir.join("note.md")).unwrap();

        // Asking the question is what makes the sweep safe: `stamp --all` calls
        // this on every document it reaches, and must leave the ones it decides
        // against byte-identical.
        assert_eq!(
            block_on(w.content_state("note.md")).unwrap(),
            ContentState::Drifted
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("note.md")).unwrap(),
            drifted,
            "content_state must not write"
        );
    }

    #[test]
    fn record_content_update_stamps_the_timestamp_field_and_the_hash_together() {
        use crate::config::Fixity;
        let dir = tempdir("content-update");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\nbody\n",
        );
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Full)
            .build();

        // A content edit at a caller-supplied instant: both the `updated` field
        // (the client's chosen name + RFC-3339 value) and the body hash land in
        // one write.
        assert!(
            block_on(w.record_content_update("note.md", Some(("updated", "2026-07-16T10:00:00Z"))))
                .unwrap()
        );
        let text = std::fs::read_to_string(dir.join("note.md")).unwrap();
        assert!(text.contains("updated: 2026-07-16T10:00:00Z"), "{text}");
        assert!(text.contains("content_hash: sha256:"), "{text}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        // The library never reads a clock: the exact string it is handed is what
        // it writes (DESIGN §2 — the client produces the instant).
        assert!(
            block_on(w.record_content_update("note.md", Some(("updated", "2099-01-01T00:00:00Z"))))
                .unwrap()
        );
        assert!(
            std::fs::read_to_string(dir.join("note.md"))
                .unwrap()
                .contains("updated: 2099-01-01T00:00:00Z")
        );
    }

    #[test]
    fn save_document_lands_the_new_text_and_both_stamps_in_one_write() {
        // The stamp-first path has to reach the same place the read-back path
        // does: new body on disk, `updated` set, `content_hash` covering the body
        // that was actually saved — and `check` agreeing the hash is true, which
        // is the assertion that would fail if the hash were taken of the wrong
        // bytes (the old body, or the text before its frontmatter was stamped).
        use crate::config::Fixity;
        let dir = tempdir("save-document");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\nbody\n",
        );
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Full)
            .build();

        let edited = "---\ntitle: Note\npart_of: index.md\n---\na different body\n";
        block_on(w.save_document("note.md", edited, Some(("updated", "2026-08-06T09:00:00Z"))))
            .unwrap();

        let text = std::fs::read_to_string(dir.join("note.md")).unwrap();
        assert!(text.contains("a different body"), "{text}");
        assert!(text.contains("updated: 2026-08-06T09:00:00Z"), "{text}");
        assert!(text.contains("content_hash: sha256:"), "{text}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn save_document_agrees_byte_for_byte_with_writing_then_recording() {
        // The change is meant to be a saving, not a difference: the same edit
        // through the old two-step route must produce the same file. Anything
        // else would be a silent format or ordering change in every document a
        // client saves.
        use crate::config::Fixity;
        let one_step = tempdir("save-equivalence-new");
        let two_step = tempdir("save-equivalence-old");
        let edited = "---\ntitle: Note\npart_of: index.md\n---\nrewritten\n";
        let stamp = Some(("updated", "2026-08-06T09:00:00Z"));

        for dir in [&one_step, &two_step] {
            write(
                dir,
                "index.md",
                "---\ntitle: Home\ncontents:\n- note.md\n---\n",
            );
            write(
                dir,
                "note.md",
                "---\ntitle: Note\npart_of: index.md\n---\nbody\n",
            );
        }

        let mut w = Workspace::builder(StdFs)
            .root(&one_step)
            .fixity(Fixity::Full)
            .build();
        block_on(w.save_document("note.md", edited, stamp)).unwrap();

        let mut old = Workspace::builder(StdFs)
            .root(&two_step)
            .fixity(Fixity::Full)
            .build();
        std::fs::write(two_step.join("note.md"), edited).unwrap();
        assert!(block_on(old.record_content_update("note.md", stamp)).unwrap());

        assert_eq!(
            std::fs::read_to_string(one_step.join("note.md")).unwrap(),
            std::fs::read_to_string(two_step.join("note.md")).unwrap(),
        );
    }

    #[test]
    fn save_document_writes_the_text_even_when_no_stamp_applies() {
        // With fixity off and no `updated` field there is nothing to stamp — which
        // makes `record_content_update` a no-op, but must not make a *save* one.
        // The text is the point; the stamps are bookkeeping around it.
        use crate::config::Fixity;
        let dir = tempdir("save-document-unstamped");
        write(&dir, "index.md", "---\ntitle: Home\n---\nold\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Off)
            .build();

        block_on(w.save_document("index.md", "---\ntitle: Home\n---\nnew\n", None)).unwrap();

        let text = std::fs::read_to_string(dir.join("index.md")).unwrap();
        assert!(text.contains("new"), "{text}");
        assert!(!text.contains("content_hash"), "{text}");
    }

    #[test]
    fn save_document_refuses_a_path_that_escapes_the_root() {
        let dir = tempdir("save-document-escape");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        let mut w = Workspace::builder(StdFs).root(&dir).build();

        let err = block_on(w.save_document("../escape.md", "---\ntitle: X\n---\n", None))
            .expect_err("an escaping path must be refused");

        assert!(
            matches!(err, prov_graph::error::Error::Escape(_)),
            "{err:?}"
        );
        assert!(!dir.parent().unwrap().join("escape.md").exists());
    }

    #[test]
    fn record_content_update_writes_a_timestamp_even_with_fixity_off() {
        // The timestamp axis is independent of fixity: `updated` tracking works
        // with no checksums at all (and writes no content_hash then).
        use crate::config::Fixity;
        let dir = tempdir("content-update-nofix");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        write(
            &dir,
            "note.md",
            "---\ntitle: Note\npart_of: index.md\n---\nbody\n",
        );
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .fixity(Fixity::Off)
            .build();

        assert!(
            block_on(
                w.record_content_update("note.md", Some(("modified", "2026-07-16T10:00:00Z")))
            )
            .unwrap()
        );
        let text = std::fs::read_to_string(dir.join("note.md")).unwrap();
        assert!(text.contains("modified: 2026-07-16T10:00:00Z"), "{text}");
        assert!(
            !text.contains("content_hash"),
            "fixity off records no hash: {text}"
        );
    }
}
