//! `confirm` — append a dated, attributed statement that someone read a
//! document and found it correct.
//!
//! The one verb that writes the `confirmed` list ([`provenance`]), and it only
//! ever appends: an entry is a fact about a moment, so a second confirmation by
//! the same actor is a second fact rather than a duplicate, and nothing here
//! rewrites or drops what is already there.
//!
//! It is a mutation in this module's sense — one staged change, one commit — and
//! deliberately *not* a content update: the `updated` stamp is what a
//! confirmation is measured against, so a verb that bumped it would make every
//! confirmation fresh by construction. Appending to the list is bookkeeping
//! about the document, not an edit of it.
//!
//! [`provenance`]: crate::provenance

use std::path::Path;

use crate::mutate::ContentState;
use crate::provenance::{CONFIRMED, Confirmation};
use crate::workspace::Workspace;
use prov_graph::error::{Error, Result};
use prov_graph::link;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Append a confirmation to the document at `path`: `by` confirmed it `at`
    /// that instant. Returns the entry as written.
    ///
    /// The caller supplies both halves — who is at the keyboard is a fact about
    /// a device, and the instant is the CLI's clock (the library stays
    /// clockless, DESIGN §2). `at` is expected in the same fixed-width RFC 3339
    /// UTC spelling as the `updated` stamp, since the two are compared as
    /// strings.
    ///
    /// For a document that records a `content_hash`, the entry also names the
    /// digest on record as `of`, so the confirmation is bound to bytes as well
    /// as to a stamp. A document whose recorded digest no longer matches its
    /// bytes is **refused**: the digest is already a
    /// [`FixityMismatch`](crate::Finding::FixityMismatch), and a confirmation
    /// cannot vouch its way past one — restamp first, then confirm what is
    /// actually there.
    ///
    /// Refused too: a `confirmed` key that holds something other than a list,
    /// since appending to it would mean deciding what the author meant by it.
    pub async fn confirm(
        &mut self,
        path: impl AsRef<Path>,
        by: &str,
        at: &str,
    ) -> Result<Confirmation> {
        let path = link::normalize(path.as_ref());
        if by.trim().is_empty() {
            return Err(Error::Structure(
                "a confirmation names who made it; an empty actor is refused".into(),
            ));
        }
        let (text, doc) = self.load(&path).await?;

        let of = match self.content_state(&path).await? {
            ContentState::Drifted => {
                return Err(Error::Structure(format!(
                    "{}: its recorded content checksum no longer matches its bytes — \
                     `prov stamp {}` first, then confirm what is there",
                    path.display(),
                    path.display(),
                )));
            }
            ContentState::Unrecorded => None,
            // Intact, or a digest this build cannot compute: either way the
            // entry names what is on record, which is what a later `check`
            // compares it against.
            ContentState::Intact | ContentState::Unverifiable => doc
                .meta
                .get("content_hash")
                .and_then(prov_graph::meta::Value::as_str)
                .map(str::to_string),
        };

        let mut entries = match doc.meta.get(CONFIRMED) {
            None | Some(prov_graph::meta::Value::Null) => Vec::new(),
            Some(prov_graph::meta::Value::Sequence(items)) => items.clone(),
            Some(_) => {
                return Err(Error::Structure(format!(
                    "{}: `{CONFIRMED}` is not a list, so nothing can be appended to it",
                    path.display()
                )));
            }
        };
        let entry = Confirmation {
            by: by.to_string(),
            at: at.to_string(),
            of,
        };
        entries.push(entry.to_value());

        let text = prov_store::edit::set_in_text(
            &text,
            doc.carrier,
            CONFIRMED,
            fig::Value::from(&prov_graph::meta::Value::Sequence(entries)),
        )?;
        let mut cs = self.change();
        cs.write(&path, text);
        self.commit(cs).await?;
        Ok(entry)
    }

    /// A document's confirmations, sorted into live and stale against its
    /// current `updated` stamp and `content_hash` — the read half of
    /// [`confirm`](Self::confirm), and what `check` reports from.
    pub async fn confirmations(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<crate::provenance::Confirmations> {
        let path = link::normalize(path.as_ref());
        let (_, doc) = self.load(&path).await?;
        Ok(crate::provenance::Confirmations::read(
            &doc.meta,
            self.updated_field(),
        ))
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::provenance::Tier;
    use crate::workspace::Settings;
    use crate::{Finding, Workspace, block_on};
    use prov_store::fs::StdFs;
    use prov_testkit::{read, scratch, write};

    fn workspace(dir: &Path) -> Workspace<StdFs> {
        Workspace::builder(StdFs)
            .root(dir)
            .settings(Settings {
                updated: "updated".into(),
                ..Default::default()
            })
            .build()
    }

    #[test]
    fn confirm_appends_and_check_reports_the_entry_an_edit_outlives() {
        let dir = scratch("confirm", "append");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(
            &dir,
            "a.md",
            "---\npart_of: index.md\nupdated: 2026-09-11T09:00:00.000000Z\n---\nalpha\n",
        );
        let mut ws = workspace(&dir);

        let entry = block_on(ws.confirm("a.md", "amh", "2026-09-11T09:20:00.000000Z")).unwrap();
        assert_eq!(entry.of, None, "a combined document has no digest to name");
        let text = read(&dir, "a.md");
        assert!(
            text.contains("confirmed:\n- by: amh\n  at: 2026-09-11T09:20:00.000000Z\n"),
            "{text}"
        );
        assert!(
            text.ends_with("---\nalpha\n"),
            "the body is untouched: {text}"
        );
        assert!(
            text.contains("updated: 2026-09-11T09:00:00.000000Z"),
            "confirming never stamps `updated`: {text}"
        );
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
        assert_eq!(
            block_on(ws.confirmations("a.md")).unwrap().tier(),
            Tier::HumanConfirmed
        );

        // A second confirmation is appended, not deduplicated.
        block_on(ws.confirm("a.md", "agent:claude-opus-5", "2026-09-11T09:30:00.000000Z")).unwrap();
        assert_eq!(block_on(ws.confirmations("a.md")).unwrap().live.len(), 2);

        // The document changes after both — the stamp moves, and `check` says
        // the newest assurance no longer holds.
        block_on(
            ws.record_content_update("a.md", Some(("updated", "2026-09-11T10:00:00.000000Z"))),
        )
        .unwrap();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            matches!(
                findings.as_slice(),
                [Finding::ConfirmationStale { doc, by, .. }]
                    if doc == Path::new("a.md") && by == "agent:claude-opus-5"
            ),
            "{findings:?}"
        );
        // Diagnosis only.
        assert_eq!(block_on(ws.remedies(&findings[0])).unwrap().len(), 0);
        assert_eq!(
            block_on(ws.confirmations("a.md")).unwrap().tier(),
            Tier::Unconfirmed
        );

        // Confirming again is the repair; the stale entries stay as history and
        // are not reported for as long as something newer stands.
        block_on(ws.confirm("a.md", "amh", "2026-09-11T10:05:00.000000Z")).unwrap();
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
        let standing = block_on(ws.confirmations("a.md")).unwrap();
        assert_eq!((standing.live.len(), standing.stale.len()), (1, 2));
        assert_eq!(standing.tier(), Tier::HumanConfirmed);
    }

    #[test]
    fn a_covered_document_binds_to_its_digest_and_a_drifted_one_is_refused() {
        let dir = scratch("confirm", "digest");
        write(&dir, "index.md", "---\ncontents:\n- a.yaml\n---\n");
        let hash = crate::fixity::digest(b"alpha\n");
        write(
            &dir,
            "a.yaml",
            format!("part_of: index.md\ncontent: a.md\ncontent_hash: {hash}\n"),
        );
        write(&dir, "a.md", "alpha\n");
        let mut ws = workspace(&dir);

        let entry = block_on(ws.confirm("a.yaml", "amh", "2026-09-11T09:20:00.000000Z")).unwrap();
        assert_eq!(entry.of.as_deref(), Some(hash.as_str()));
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);

        // The payload changes out of band: fixity mismatch, and a confirmation
        // cannot be made over it until the checksum is restated.
        std::fs::write(dir.join("a.md"), "beta\n").unwrap();
        let err = block_on(ws.confirm("a.yaml", "amh", "2026-09-11T09:30:00.000000Z")).unwrap_err();
        assert!(err.to_string().contains("prov stamp"), "{err}");

        // Restamped, the digest on record moves and the old entry is stale.
        block_on(ws.record_content_update("a.yaml", None)).unwrap();
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            findings.iter().any(|f| matches!(f, Finding::ConfirmationStale { doc, .. } if doc == Path::new("a.yaml"))),
            "{findings:?}"
        );
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, Finding::FixityMismatch { .. })),
            "{findings:?}"
        );
    }

    #[test]
    fn a_confirmed_key_that_is_not_a_list_is_refused() {
        let dir = scratch("confirm", "not-a-list");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(
            &dir,
            "a.md",
            "---\npart_of: index.md\nconfirmed: yes\n---\n",
        );
        let mut ws = workspace(&dir);
        let err = block_on(ws.confirm("a.md", "amh", "2026-09-11T09:20:00.000000Z")).unwrap_err();
        assert!(err.to_string().contains("not a list"), "{err}");
        assert_eq!(
            read(&dir, "a.md"),
            "---\npart_of: index.md\nconfirmed: yes\n---\n"
        );
        // And `check` says nothing about it: the key is read for what it can
        // say, and it says nothing.
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
    }
}
