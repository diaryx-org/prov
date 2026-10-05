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
//! An entry may carry keys of another tool's beside `by`, `at` and `of` — a
//! signature over the entry, say — written by [`confirm_with`] at the moment
//! the entry is made, since nothing later may rewrite it. prov keeps them and
//! never reads them.
//!
//! [`provenance`]: crate::provenance
//! [`confirm_with`]: Workspace::confirm_with

use std::path::Path;

use crate::config::ConfirmationBinding;
use crate::mutate::ContentState;
use crate::provenance::{CONFIRMED, Confirmation};
use crate::workspace::Workspace;
use prov_graph::error::{Error, Result};
use prov_graph::link;
use prov_graph::meta::{Mapping, Value};
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

/// The keys of an entry that prov writes and reads.
const PROV_KEYS: [&str; 3] = ["by", "at", "of"];

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
    /// In a workspace that sets `confirmations: content`
    /// ([`confirmation_binding`](Self::confirmation_binding)), every other
    /// document's entry names its [content digest](Self::content_digest) as
    /// `of` — the digest of the document as written, without its `confirmed`
    /// list — so the confirmation stands exactly while the content does.
    ///
    /// Refused too: a `confirmed` key that holds something other than a list,
    /// since appending to it would mean deciding what the author meant by it.
    pub async fn confirm(
        &mut self,
        path: impl AsRef<Path>,
        by: &str,
        at: &str,
    ) -> Result<Confirmation> {
        self.confirm_with(path, by, at, |_| Ok(Mapping::new()))
            .await
    }

    /// [`confirm`](Self::confirm), with keys of the caller's own written into
    /// the entry beside `by`, `at` and `of`.
    ///
    /// `extend` is handed the entry exactly as it will be written — its `of`
    /// already named — and answers with the keys to add: a signature over the
    /// entry is the case this exists for, and a signature has to cover the
    /// `of` that only this verb computes. They are written now or never,
    /// because nothing rewrites an entry once it is in the list.
    ///
    /// prov keeps such keys and never reads them: they decide nothing about
    /// whether the entry is well formed, whether it stands, or the document's
    /// tier, and they sit inside the list the content digest leaves out.
    /// [`Confirmation::read_extended`] hands them back. A key that is one of
    /// prov's own (`by`, `at`, `of`) is refused rather than let it say
    /// something prov would read; so is an error from `extend`, with nothing
    /// written.
    pub async fn confirm_with(
        &mut self,
        path: impl AsRef<Path>,
        by: &str,
        at: &str,
        extend: impl FnOnce(&Confirmation) -> Result<Mapping>,
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

        let entries = match doc.meta.get(CONFIRMED) {
            None | Some(prov_graph::meta::Value::Null) => Vec::new(),
            Some(prov_graph::meta::Value::Sequence(items)) => items.clone(),
            Some(_) => {
                return Err(Error::Structure(format!(
                    "{}: `{CONFIRMED}` is not a list, so nothing can be appended to it",
                    path.display()
                )));
            }
        };
        let append = |entry: &Confirmation, extra: &Mapping| {
            let mut entries = entries.clone();
            let mut value = entry.to_value();
            if let Value::Mapping(map) = &mut value {
                map.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            entries.push(value);
            prov_store::edit::set_in_text(
                &text,
                doc.carrier,
                CONFIRMED,
                fig::Value::from(&prov_graph::meta::Value::Sequence(entries)),
            )
        };
        let mut entry = Confirmation {
            by: by.to_string(),
            at: at.to_string(),
            of,
        };
        if entry.of.is_none() && self.confirmation_binding() == ConfirmationBinding::Content {
            // Taken from the text as it will be written, not as it was read: a
            // document with no metadata block gains one here, and the content
            // digest is what a reader computes from the file that results. The
            // entry's own `of` sits inside the list the digest leaves out, so
            // naming it does not move it — and nor do the caller's keys.
            let provisional = append(&entry, &Mapping::new())?;
            entry.of = Some(crate::provenance::content_digest(&path, &provisional)?);
        }
        let extra = extend(&entry)?;
        if let Some(own) = PROV_KEYS.iter().find(|k| extra.contains_key(**k)) {
            return Err(Error::Structure(format!(
                "`{own}` is prov's own key in a confirmation, and cannot be added beside it"
            )));
        }
        let written = append(&entry, &extra)?;
        let mut cs = self.change();
        cs.write(&path, written);
        self.commit(cs).await?;
        Ok(entry)
    }

    /// A document's confirmations, sorted into live and stale against its
    /// current `updated` stamp and `content_hash` — and, in a workspace that
    /// sets `confirmations: content`, its [content digest](Self::content_digest).
    /// The read half of [`confirm`](Self::confirm), and what `check` reports
    /// from.
    pub async fn confirmations(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<crate::provenance::Confirmations> {
        let path = link::normalize(path.as_ref());
        let (text, doc) = self.load(&path).await?;
        Ok(self.standing(&text, &doc))
    }

    /// The **content digest** of the document at `path`: its digest without
    /// its `confirmed` list — see
    /// [`provenance::content_digest`](crate::provenance::content_digest),
    /// which computes the same over any text, such as the bytes the document
    /// had at a past revision.
    pub async fn content_digest(&self, path: impl AsRef<Path>) -> Result<String> {
        let path = link::normalize(path.as_ref());
        let (text, doc) = self.load(&path).await?;
        crate::provenance::content_digest_of(&text, &doc)
    }

    /// Sort a loaded document's confirmations by this workspace's rule: the
    /// one place [`confirmations`](Self::confirmations) and `check` share.
    ///
    /// The content digest is computed only where it can decide something — a
    /// workspace bound to content, a document with a list and no
    /// `content_hash`. One that cannot be computed is left out, which leaves
    /// an entry naming an `of` stale: nothing says it still describes the
    /// document.
    pub(crate) fn standing(
        &self,
        text: &str,
        doc: &prov_graph::document::Document,
    ) -> crate::provenance::Confirmations {
        let digest = (self.confirmation_binding() == ConfirmationBinding::Content
            && doc
                .meta
                .get("content_hash")
                .and_then(prov_graph::meta::Value::as_str)
                .is_none()
            && doc.meta.get(CONFIRMED).is_some())
        .then(|| crate::provenance::content_digest_of(text, doc).ok())
        .flatten();
        crate::provenance::Confirmations::read_against(
            &doc.meta,
            self.updated_field(),
            digest.as_deref(),
        )
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

    fn content_workspace(dir: &Path) -> Workspace<StdFs> {
        Workspace::builder(StdFs)
            .root(dir)
            .settings(Settings {
                updated: "updated".into(),
                confirmations: ConfirmationBinding::Content,
                ..Default::default()
            })
            .build()
    }

    /// The content digest of `text` at `path`.
    fn digest_of(path: &str, text: &str) -> String {
        crate::provenance::content_digest(path, text).unwrap()
    }

    /// Confirming moves nothing the content digest covers, in any of the
    /// carriers a document's metadata can have; every other edit moves it.
    fn content_digest_ignores_only_confirmations(name: &str, path: &str, text: &str) {
        let dir = scratch("confirm", name);
        write(&dir, "index.md", format!("---\ncontents:\n- {path}\n---\n"));
        write(&dir, path, text);
        let mut ws = workspace(&dir);

        let before = block_on(ws.content_digest(path)).unwrap();
        assert_eq!(
            before,
            crate::fixity::digest(text.as_bytes()),
            "no `confirmed` key: the text itself"
        );
        block_on(ws.confirm(path, "amh", "2026-09-11T09:20:00.000000Z")).unwrap();
        let once = read(&dir, path);
        assert_ne!(once, text, "an entry was written");
        assert_eq!(digest_of(path, &once), before, "the first entry: {once}");
        block_on(ws.confirm(path, "agent:claude-opus-5", "2026-09-11T09:30:00.000000Z")).unwrap();
        let twice = read(&dir, path);
        assert_eq!(digest_of(path, &twice), before, "a further entry: {twice}");
        assert_eq!(block_on(ws.content_digest(path)).unwrap(), before);
    }

    #[test]
    fn the_content_digest_leaves_out_the_confirmed_list_and_nothing_else() {
        let yaml = "---\npart_of: index.md # the parent\ntitle: A\nupdated: 2026-09-11T09:00:00.000000Z\n---\nalpha\n";
        content_digest_ignores_only_confirmations("digest-yaml", "a.md", yaml);

        // Confirmed, then edited in each of the ways that are not confirming.
        let confirmed = |text: &str| {
            prov_store::edit::set_in_text(
                text,
                prov_graph::document::Document::parse("a.md", text)
                    .unwrap()
                    .carrier,
                CONFIRMED,
                fig::Value::from(
                    &Confirmation {
                        by: "amh".into(),
                        at: "2026-09-11T09:20:00.000000Z".into(),
                        of: None,
                    }
                    .to_value(),
                ),
            )
            .unwrap()
        };
        let base = digest_of("a.md", &confirmed(yaml));
        assert_eq!(base, digest_of("a.md", yaml));
        for (what, edited) in [
            ("the body", yaml.replace("alpha", "beta")),
            ("another field", yaml.replace("title: A", "title: B")),
            (
                "the stamp",
                yaml.replace("09:00:00.000000Z", "10:00:00.000000Z"),
            ),
            ("a comment", yaml.replace("# the parent", "# its parent")),
        ] {
            assert_ne!(
                digest_of("a.md", &confirmed(&edited)),
                base,
                "editing {what} moves the content digest"
            );
        }
    }

    #[test]
    fn the_content_digest_of_a_document_with_no_metadata_block() {
        content_digest_ignores_only_confirmations("digest-bare", "a.md", "alpha\n");
    }

    #[test]
    fn the_content_digest_of_a_whole_file_document() {
        content_digest_ignores_only_confirmations(
            "digest-whole",
            "a.yaml",
            "# a node\npart_of: index.md\ntitle: A\n",
        );
    }

    #[cfg(feature = "toml")]
    #[test]
    fn the_content_digest_of_toml_frontmatter() {
        content_digest_ignores_only_confirmations(
            "digest-toml",
            "a.md",
            "+++\n# the parent\npart_of = \"index.md\"\ntitle = \"A\"\n+++\nalpha\n",
        );
    }

    /// JSON frontmatter, over a list written by hand: fig cannot yet splice a
    /// mapping into a JSON sequence, so `confirm` cannot write one there.
    #[cfg(feature = "json")]
    #[test]
    fn the_content_digest_of_json_frontmatter() {
        let bare = ";;;\n{\n  \"part_of\": \"index.md\",\n  \"title\": \"A\"\n}\n;;;\nalpha\n";
        let confirmed = ";;;\n{\n  \"part_of\": \"index.md\",\n  \"title\": \"A\",\n  \"confirmed\": [{\"by\": \"amh\", \"at\": \"2026-09-11T09:20:00.000000Z\"}]\n}\n;;;\nalpha\n";
        assert_eq!(
            digest_of("a.md", bare),
            crate::fixity::digest(bare.as_bytes())
        );
        assert_eq!(digest_of("a.md", confirmed), digest_of("a.md", bare));
        assert_ne!(
            digest_of("a.md", &confirmed.replace("alpha", "beta")),
            digest_of("a.md", bare)
        );
    }

    /// A caller's keys ride in the entry they were made with, see the entry
    /// as written (its `of` included), and change nothing prov decides: the
    /// digest, the standing, the tier, `check`. One of prov's own keys is
    /// refused, and so is a caller that fails, each with nothing written.
    #[test]
    fn a_callers_keys_are_written_with_the_entry_and_never_read() {
        let dir = scratch("confirm", "extend");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        let original = "---\npart_of: index.md\nupdated: 2026-09-11T09:00:00.000000Z\n---\nalpha\n";
        write(&dir, "a.md", original);
        let mut ws = content_workspace(&dir);

        let entry =
            block_on(
                ws.confirm_with("a.md", "amh", "2026-09-11T09:20:00.000000Z", |entry| {
                    let mut signature = Mapping::new();
                    signature.insert("key".into(), Value::String("RWQkey".into()));
                    signature.insert(
                        "over".into(),
                        Value::String(format!(
                            "{} {}",
                            entry.by,
                            entry.of.as_deref().unwrap_or("-")
                        )),
                    );
                    let mut extra = Mapping::new();
                    extra.insert("signature".into(), Value::Mapping(signature));
                    Ok(extra)
                }),
            )
            .unwrap();
        let digest = block_on(ws.content_digest("a.md")).unwrap();
        assert_eq!(digest, crate::fixity::digest(original.as_bytes()));
        assert_eq!(entry.of.as_deref(), Some(digest.as_str()));

        let text = read(&dir, "a.md");
        assert!(
            text.contains(&format!("    over: amh {digest}\n")),
            "{text}"
        );
        let doc = prov_graph::document::Document::parse(Path::new("a.md"), &text).unwrap();
        let extended = Confirmation::read_extended(&doc.meta);
        assert_eq!(extended.len(), 1);
        assert_eq!(extended[0].0, entry);
        assert_eq!(extended[0].1.keys().collect::<Vec<_>>(), ["signature"]);
        assert_eq!(Confirmation::read_all(&doc.meta), vec![entry]);
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
        assert_eq!(
            block_on(ws.confirmations("a.md")).unwrap().tier(),
            Tier::HumanConfirmed
        );

        let before = read(&dir, "a.md");
        let refused =
            block_on(
                ws.confirm_with("a.md", "amh", "2026-09-11T09:30:00.000000Z", |_| {
                    let mut extra = Mapping::new();
                    extra.insert("of".into(), Value::String("sha256:mine".into()));
                    Ok(extra)
                }),
            );
        assert!(refused.is_err());
        let failed = block_on(ws.confirm_with(
            "a.md",
            "amh",
            "2026-09-11T09:30:00.000000Z",
            |_| Err(Error::Structure("no key to sign with".into())),
        ));
        assert!(failed.is_err());
        assert_eq!(read(&dir, "a.md"), before, "nothing written");
    }

    #[test]
    fn bound_to_content_a_confirmation_stands_while_the_content_digest_does() {
        let dir = scratch("confirm", "content");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        let original = "---\npart_of: index.md\nupdated: 2026-09-11T09:00:00.000000Z\n---\nalpha\n";
        write(&dir, "a.md", original);
        let mut ws = content_workspace(&dir);

        let entry = block_on(ws.confirm("a.md", "amh", "2026-09-11T09:20:00.000000Z")).unwrap();
        let digest = block_on(ws.content_digest("a.md")).unwrap();
        assert_eq!(entry.of.as_deref(), Some(digest.as_str()));
        assert_eq!(digest, crate::fixity::digest(original.as_bytes()));
        let text = read(&dir, "a.md");
        assert!(text.contains(&format!("  of: {digest}\n")), "{text}");
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
        assert_eq!(
            block_on(ws.confirmations("a.md")).unwrap().tier(),
            Tier::HumanConfirmed
        );
        // A further entry names the same digest, and both stand.
        let again = block_on(ws.confirm("a.md", "agent:x", "2026-09-11T09:30:00.000000Z")).unwrap();
        assert_eq!(again.of, entry.of);
        assert_eq!(block_on(ws.confirmations("a.md")).unwrap().live.len(), 2);

        // An edit that never bumps the stamp — invisible to the stamp rule —
        // unseats both.
        let edited = read(&dir, "a.md").replace("alpha", "beta");
        std::fs::write(dir.join("a.md"), &edited).unwrap();
        let standing = block_on(ws.confirmations("a.md")).unwrap();
        assert_eq!((standing.live.len(), standing.stale.len()), (0, 2));
        let findings = block_on(ws.check("index.md")).unwrap();
        assert!(
            matches!(
                findings.as_slice(),
                [Finding::ConfirmationStale { doc, by, .. }]
                    if doc == Path::new("a.md") && by == "agent:x"
            ),
            "{findings:?}"
        );
        // Undone byte for byte, they stand again: the digest is the record.
        std::fs::write(dir.join("a.md"), edited.replace("beta", "alpha")).unwrap();
        assert_eq!(block_on(ws.confirmations("a.md")).unwrap().live.len(), 2);

        // A stamp moved alone is a change of content too.
        block_on(
            ws.record_content_update("a.md", Some(("updated", "2026-09-11T10:00:00.000000Z"))),
        )
        .unwrap();
        assert_eq!(block_on(ws.confirmations("a.md")).unwrap().live.len(), 0);

        // And an entry dated before the stamp still stands while its digest
        // does: the stamp is not consulted for it.
        let mut ws = content_workspace(&dir);
        block_on(ws.confirm("a.md", "amh", "2026-09-11T08:00:00.000000Z")).unwrap();
        let standing = block_on(ws.confirmations("a.md")).unwrap();
        assert_eq!(standing.live.len(), 1, "{standing:?}");
        assert_eq!(block_on(ws.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn bound_to_content_an_entry_without_of_keeps_the_stamp_rule() {
        let dir = scratch("confirm", "content-legacy");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(
            &dir,
            "a.md",
            "---\npart_of: index.md\nupdated: 2026-09-11T09:00:00.000000Z\nconfirmed:\n- by: amh\n  at: 2026-09-11T09:20:00.000000Z\n---\nalpha\n",
        );
        let ws = content_workspace(&dir);
        assert_eq!(block_on(ws.confirmations("a.md")).unwrap().live.len(), 1);
        // An edit outside prov that never bumps the stamp is invisible to it,
        // as it always was.
        let text = read(&dir, "a.md").replace("alpha", "beta");
        std::fs::write(dir.join("a.md"), text).unwrap();
        assert_eq!(block_on(ws.confirmations("a.md")).unwrap().live.len(), 1);
        // The stamp moving is what unseats it.
        let text = read(&dir, "a.md").replace("09:00:00.000000Z", "10:00:00.000000Z");
        std::fs::write(dir.join("a.md"), text).unwrap();
        assert_eq!(block_on(ws.confirmations("a.md")).unwrap().stale.len(), 1);
    }

    #[test]
    fn bound_to_content_a_covered_document_still_names_its_content_hash() {
        let dir = scratch("confirm", "content-covered");
        write(&dir, "index.md", "---\ncontents:\n- a.yaml\n---\n");
        let hash = crate::fixity::digest(b"alpha\n");
        write(
            &dir,
            "a.yaml",
            format!("part_of: index.md\ncontent: a.md\ncontent_hash: {hash}\n"),
        );
        write(&dir, "a.md", "alpha\n");
        let mut ws = content_workspace(&dir);
        let entry = block_on(ws.confirm("a.yaml", "amh", "2026-09-11T09:20:00.000000Z")).unwrap();
        assert_eq!(entry.of.as_deref(), Some(hash.as_str()));
        assert_eq!(block_on(ws.confirmations("a.yaml")).unwrap().live.len(), 1);
    }

    /// A workspace that does not bind to content reads an `of` on an ordinary
    /// document as it always did: a digest the document does not record, so
    /// the entry does not stand.
    #[test]
    fn not_bound_to_content_an_of_on_an_ordinary_document_is_read_as_before() {
        let dir = scratch("confirm", "content-off");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(
            &dir,
            "a.md",
            "---\npart_of: index.md\nupdated: 2026-09-11T09:00:00.000000Z\n---\nalpha\n",
        );
        block_on(content_workspace(&dir).confirm("a.md", "amh", "2026-09-11T09:20:00.000000Z"))
            .unwrap();
        assert_eq!(
            block_on(content_workspace(&dir).confirmations("a.md"))
                .unwrap()
                .live
                .len(),
            1
        );
        let standing = block_on(workspace(&dir).confirmations("a.md")).unwrap();
        assert_eq!((standing.live.len(), standing.stale.len()), (0, 1));
        // And confirming there writes no `of`.
        let entry = block_on(workspace(&dir).confirm("a.md", "amh", "2026-09-11T09:30:00.000000Z"))
            .unwrap();
        assert_eq!(entry.of, None);
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
