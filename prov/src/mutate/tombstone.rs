//! The deletion log: what [`delete`](super::delete) records, and `restore`.
//!
//! prov does not keep the bytes of a deleted document. Recovering those is the
//! job of whatever version-control or backup tool the workspace is kept under —
//! the same division `prov ignore` draws, where prov says which files *are* the
//! workspace and names no tool to record them.
//!
//! What no such tool has is the other half. A `prov rm` touches three documents:
//! the file itself, the parent's spanning entry for it, and the registry the id
//! is retired from. Bring the file back with `git checkout` and you have a
//! document whose `part_of` names a parent that does not list it, carrying an id
//! nothing registers — restored bytes in a graph that no longer admits them.
//! Repairing that is prov's knowledge and nothing else's, so a delete writes
//! down what it would take: the path, the title, the id, the parent, and the
//! prose body that went with it.
//!
//! The log is a first-class, *reachable* member — a record store the root links
//! through the deletions relation, which `check` validates like any other. It
//! parks nothing. `restore` reads one record and repairs the graph around a file
//! the caller has already put back.
//!
//! # The bin this replaced
//!
//! Before the log there was a recycle bin, which moved the bytes under an
//! unreached `recyclebin/items/` and moved them back. A workspace that still
//! declares one keeps working: the legacy `recycle_bin` pointer resolves
//! ([`deletions_pointer`](crate::Workspace::deletions_pointer)), `items/` stays
//! parked out of every walk, and `restore` still moves parked bytes home when
//! the record names them. Nothing writes a new one.

use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;
use prov_graph::document::Document;
use prov_graph::error::{Error, Result};
use prov_graph::graph::Target;
use prov_graph::link::{self, Link};
use prov_graph::meta::Value;
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use crate::change::ChangeSet;

/// The default title a log document is created with.
const LOG_TITLE: &str = "Deletions";

/// What a delete destroyed, as the log records it.
///
/// Every field is something `restore` needs and the file itself cannot supply
/// once it is gone — or, in `title`'s case, something it *could* supply but only
/// after it is back, which is too late to author the parent's entry with.
pub(crate) struct Deletion {
    /// The document's title, for the parent entry `restore` re-authors.
    pub title: String,
    /// The id the document held, retired from the registry by the delete and
    /// re-registered by `restore`. `None` when it had none.
    pub id: Option<prov_graph::identity::Id>,
    /// The workspace-relative path it was deleted from — the record's identity,
    /// and what a caller names to restore it.
    pub from: PathBuf,
    /// The parent whose spanning entry was removed, if it had one.
    pub parent: Option<PathBuf>,
    /// The prose body or record store that travelled with it, if one did.
    pub body: Option<PathBuf>,
    /// A caller-supplied deletion timestamp. The library takes it as an argument
    /// rather than reading a clock, so the op stays deterministic.
    pub at: Option<String>,
}

impl Deletion {
    /// The record as it is written into the log.
    fn to_record(&self) -> Value {
        let mut record = prov_graph::meta::Mapping::new();
        let mut put = |key: &str, path: &Path| {
            record.insert(
                key.into(),
                Value::String(path.to_string_lossy().into_owned()),
            );
        };
        put("from", &self.from);
        if let Some(parent) = &self.parent {
            put("parent", parent);
        }
        if let Some(body) = &self.body {
            put("body", body);
        }
        record.insert("title".into(), Value::String(self.title.clone()));
        if let Some(id) = &self.id {
            record.insert("id".into(), Value::String(id.to_string()));
        }
        if let Some(at) = &self.at {
            record.insert("at".into(), Value::String(at.clone()));
        }
        Value::Mapping(record)
    }
}

/// One record as `restore` and `clear_deletions` read it back.
///
/// Both legacy spellings are accepted: `body_from` for `body`, and `bin` /
/// `body_bin` for the paths a recycle bin parked its bytes at. A record from a
/// bin therefore restores by *moving the bytes home*, exactly as it always did,
/// while a record from a log restores by repairing the graph around bytes the
/// caller has already put back.
struct Record {
    parent: Option<PathBuf>,
    body: Option<PathBuf>,
    title: Option<String>,
    id: Option<prov_graph::identity::Id>,
    /// Where a recycle bin parked the document, for an unmigrated record only.
    parked: Option<PathBuf>,
    /// Where it parked the body, likewise.
    parked_body: Option<PathBuf>,
}

impl Record {
    fn parse(value: &Value) -> Option<Self> {
        let field = |key: &str| value.get(key).and_then(Value::as_str);
        let path = |key: &str| field(key).map(PathBuf::from);
        // `from` is the record's identity: a record without one names no
        // document and is not one. The caller already holds the value — it is
        // what it looked this record up by — so it is required here, not carried.
        field("from")?;
        Some(Self {
            parent: path("parent"),
            body: path("body").or_else(|| path("body_from")),
            title: field("title").map(str::to_owned),
            id: field("id").map(|s| prov_graph::identity::Id(s.to_string())),
            parked: path("bin"),
            parked_body: path("body_bin"),
        })
    }
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Stage `deletion` into the workspace's deletion log, inside the change set
    /// the delete is already assembling — so the file's removal, the parent's
    /// edit and the record land together or not at all.
    ///
    /// Returns the root document's new text when the root gained a pointer to a
    /// log created by this call, and `None` otherwise. The caller owns that
    /// write, because it may have an edit of its own to the same document (the
    /// root is very often the deleted document's parent) and the two have to be
    /// one rendering.
    ///
    /// `root` is the spanning root the subject hangs from. When it *is* the
    /// subject — an orphan, which `check` reports and users routinely delete, or
    /// a separated body under `--force` — there is no reachable root to link
    /// from, so the log is written unlinked and the next delete from a real node
    /// adopts it. Writing the pointer there would stage a write to the path this
    /// same change set is removing, which recreates the file: the user sees the
    /// document still sitting there, now carrying machinery it never had.
    pub(crate) async fn stage_deletion(
        &self,
        cs: &mut ChangeSet,
        root: &Path,
        deletion: &Deletion,
        root_base: Option<String>,
    ) -> Result<Option<String>> {
        let format = self.default_embed_format();
        let ext = prov_graph::document::whole_file_extension(format);

        // The log the root points at, or — when it points at none — the
        // conventional path, which may still hold a log left unlinked by a
        // delete from an orphan. Adopting it is what keeps that case from
        // wedging every later delete against a file it expected to be absent.
        let linked = self.deletions_pointer(root).await?;
        let log = match &linked {
            Some((path, _)) => path.clone(),
            None => PathBuf::from("deletions").join(format!("index.{ext}")),
        };
        let present = linked.is_some() || self.exists(&log).await?;

        // The log's current records, and its own title and back-link, so a
        // wholesale re-render preserves them.
        let (read, mut records, title, part_of) = if present {
            let (text, doc) = self.load(&log).await?;
            let (records, title, part_of) = self.read_log(&log, &doc)?;
            (Some(text), records, title, part_of)
        } else {
            (None, Vec::new(), LOG_TITLE.to_string(), None)
        };
        records.push(deletion.to_record());

        // The index this record list was read from — or its verified absence,
        // on the bootstrap — must still hold at apply, or a re-render over a
        // drifted log would silently drop whatever a racing delete just added.
        match read {
            Some(read) => {
                cs.expect(&log, read);
            }
            None => {
                cs.expect_absent(&log);
            }
        }
        cs.write(
            &log,
            render_log(&title, part_of.as_deref(), records, format)?,
        );

        // The root's pointer, authored the first time only — and never when the
        // root is the document being deleted.
        if linked.is_some() || root == deletion.from {
            return Ok(None);
        }
        let base = match root_base {
            Some(text) => text,
            None => self.load(root).await?.0,
        };
        let root_doc = Document::parse(root, &base)?;
        let relation = self
            .relations()
            .deletions_relation()
            .ok_or_else(|| Error::Structure("no deletions relation configured".into()))?
            .to_string();
        let style = self.reference_style_for(&relation).path_style;
        let pointer = link::path_text(style, root, &log);
        Ok(Some(prov_store::edit::set_in_text(
            &base,
            root_doc.carrier,
            &relation,
            prov_store::edit::infer_scalar(&pointer),
        )?))
    }

    /// Repair the graph around a document that has been brought back to the path
    /// it was deleted from — the inverse of the record
    /// [`delete`](Self::delete) wrote.
    ///
    /// prov did not keep the bytes, so putting them back is the caller's step:
    /// `git checkout`, a restore from backup, an editor's undo history. What
    /// this does is the half that has no other owner — re-register the id the
    /// delete retired, and re-add the parent's spanning entry, which is all the
    /// delete took from the parent. (Only the parent → child direction was lost;
    /// the child's own `part_of` came back with the file, so it is correct again
    /// the moment the file is home.) The record is then dropped from the log. It
    /// all lands as one journaled [`ChangeSet`].
    ///
    /// Refuses when nothing is at `from`, naming what to do about it; when `from`
    /// is not in the log; and when the id cannot be re-registered because the
    /// workspace has since given it — or the path — to something else. That last
    /// refusal is the point of checking rather than overwriting: a sync can land
    /// a document that spells the id while the record sat in the log, and taking
    /// it back would leave that document's own frontmatter claiming an id it no
    /// longer holds. Only the author can say which should keep it.
    ///
    /// A record written by the **recycle bin** this replaced names bytes parked
    /// under `items/`, and restores the way it always did: the parked file is
    /// moved home as part of the same change set, so nothing has to be put back
    /// first.
    ///
    /// `root_doc` names the workspace root, from which the log is discovered.
    pub async fn restore(&mut self, from: &Path, root_doc: &Path) -> Result<()> {
        let from = link::normalize(from);
        let (spanning, _) = self.spanning_pair()?;
        let log = self
            .deletions_path(root_doc)
            .await?
            .ok_or_else(|| Error::Structure("workspace has no deletion log".into()))?;
        let (read, log_doc) = self.load(&log).await?;
        let (records, log_title, part_of) = self.read_log(&log, &log_doc)?;

        let from_str = from.to_string_lossy();
        let pos = records
            .iter()
            .position(|r| r.get("from").and_then(Value::as_str) == Some(from_str.as_ref()))
            .ok_or_else(|| {
                Error::Structure(format!("{} is not in the deletion log", from.display()))
            })?;
        let record = Record::parse(&records[pos]).ok_or_else(|| {
            Error::Structure(format!(
                "the deletion record for {} has no `from` path",
                from.display()
            ))
        })?;
        // The restored document's own title, for the parent entry re-authored
        // below — not the log's, which `log_title` holds.
        let title = record.title.unwrap_or_else(|| link::path_to_title(&from));

        // Where the bytes are coming from decides what this verb owes. An
        // unmigrated bin record parks them and moves them home; a log record
        // parks nothing, so the caller has to have put them back already, and
        // saying so is more use than a rename failing on a file that is not
        // there.
        let parked = match &record.parked {
            Some(parked) if self.exists(parked).await? => Some(parked.clone()),
            _ => None,
        };
        match (&parked, self.exists(&from).await?) {
            (Some(_), true) => {
                return Err(Error::Structure(format!(
                    "{} already exists; cannot restore over it",
                    from.display()
                )));
            }
            (None, false) => {
                return Err(Error::Structure(format!(
                    "nothing is at {}, and prov did not keep its bytes — put the \
                     file back first (from version control or a backup), then \
                     restore to re-register its id and relink its parent",
                    from.display()
                )));
            }
            _ => {}
        }

        // The id the record retired, against the document actually at `from`.
        // A restored file that spells a *different* id is not the document this
        // record is about, and re-registering across that would take an id from
        // a document whose own frontmatter still claims it.
        let id = record.id;
        if let Some(id) = &id {
            if let Some(returned) = self.returned_id(&from, parked.as_deref()).await?
                && returned != *id
            {
                return Err(Error::Structure(format!(
                    "{} carries id {returned}, but the deletion record names {id} \
                     — restore it by hand, or remove the record",
                    from.display()
                )));
            }
            if let Some(conflict) = self.registration_conflict(id, &from) {
                return Err(conflict.into());
            }
        }

        let mut remaining = records;
        remaining.remove(pos);
        let format = self.default_embed_format();
        let log_text = render_log(&log_title, part_of.as_deref(), remaining, format)?;

        let mut cs = self.change();
        // Re-register the ID *after* `change`'s checkpoint, so authoring the
        // parent link below reuses the document's own id rather than minting a
        // new one, and so a failure rolls the re-registration back with
        // everything else.
        if let Some(id) = &id {
            self.index_mut().register(id, &from);
        }
        // The log this record was cut from must still hold, or the re-render
        // would drop what a racing delete just recorded.
        cs.expect(&log, read);
        // An unmigrated bin record moves its parked bytes home. The slot the
        // exists-check above found free must still be free at apply — `rename`
        // overwrites, and the racer's file would be gone.
        if let Some(parked) = &parked {
            cs.expect_absent(&from);
            cs.rename(parked, &from);
            if let (Some(body), Some(parked_body)) = (&record.body, &record.parked_body)
                && self.exists(parked_body).await?
            {
                cs.rename(parked_body, body);
            }
        }
        cs.write(&log, log_text);

        // Re-add the parent's spanning entry — its removal is all the delete did
        // to the parent. Skipped when the parent is itself gone, or already
        // links the child (a hand repair, or a version-control restore that
        // brought the parent back too).
        if let Some(parent) = &record.parent
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
                    .authored_target(&spanning, parent, &from, &title, parked.is_none())
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

    /// Forget every deletion the log records. Returns how many records went.
    ///
    /// The records are the last evidence of what was deleted, and dropping them
    /// forecloses [`restore`](Self::restore) for good — so this is always
    /// explicit, never something a delete does on its own. The log document
    /// itself stays, still linked from the root, holding an empty list.
    ///
    /// ID tombstones are untouched: an id retired at deletion stays retired, so
    /// a `colophon:<id>` reference to a forgotten document remains diagnosable
    /// rather than silently reissuable.
    ///
    /// A workspace still on the **recycle bin** this replaced has bytes parked
    /// under `items/`, and those are destroyed here — which is what emptying a
    /// bin always meant, and the only thing in prov that destroys bytes it was
    /// keeping.
    pub async fn clear_deletions(&mut self, root_doc: &Path) -> Result<usize> {
        let log = self
            .deletions_path(root_doc)
            .await?
            .ok_or_else(|| Error::Structure("workspace has no deletion log".into()))?;
        let (read, log_doc) = self.load(&log).await?;
        let (records, title, part_of) = self.read_log(&log, &log_doc)?;
        let count = records.len();

        let format = self.default_embed_format();
        let log_text = render_log(&title, part_of.as_deref(), Vec::new(), format)?;

        let mut cs = self.change();
        // This forgets exactly the records this reading held: expected, so a
        // record a racing delete adds in the compute→apply gap refuses
        // ([`Error::Drifted`]) instead of being wiped from the log while its
        // parked bytes — never in `records` — survive orphaned.
        cs.expect(&log, read);
        for record in &records {
            for key in ["bin", "body_bin"] {
                if let Some(path) = record.get(key).and_then(Value::as_str) {
                    let parked = PathBuf::from(path);
                    if self.exists(&parked).await? {
                        cs.remove(parked);
                    }
                }
            }
        }
        cs.write(&log, log_text);
        self.commit(cs).await?;
        Ok(count)
    }

    /// A log document's records, its title, and its back-link if it carries one.
    ///
    /// The log is a record store, so a markdown carrier is refused here the way
    /// it is for the registry (DESIGN §5, the whole-file rule).
    fn read_log(
        &self,
        path: &Path,
        doc: &Document,
    ) -> Result<(Vec<Value>, String, Option<String>)> {
        if let Some(carrier) = doc.carrier {
            prov_graph::document::require_whole_file(path, carrier)?;
        }
        let records = doc
            .meta
            .get("deleted")
            .and_then(Value::as_sequence)
            .map(<[Value]>::to_vec)
            .unwrap_or_default();
        Ok((records, title_of(doc), part_of_of(doc)))
    }

    /// The id spelled by the document `restore` is about to re-register — read
    /// from wherever its bytes currently are, which for an unmigrated bin record
    /// is still the parked copy.
    ///
    /// `None` when the document declares none, which is the ordinary case under
    /// `id_storage: registry`: there is nothing to disagree with, so the
    /// record's id stands.
    async fn returned_id(
        &self,
        from: &Path,
        parked: Option<&Path>,
    ) -> Result<Option<prov_graph::identity::Id>> {
        let at = parked.unwrap_or(from);
        let Ok((_, doc)) = self.load(at).await else {
            return Ok(None);
        };
        Ok(doc
            .meta
            .get("id")
            .and_then(Value::as_str)
            .map(|s| prov_graph::identity::Id(s.to_string())))
    }
}

/// A log document's title, defaulting for one that carries none.
fn title_of(doc: &Document) -> String {
    doc.meta
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(LOG_TITLE)
        .to_string()
}

/// A log document's `part_of`, if it carries one.
///
/// The log is machinery, reached one-way through the root's pointer, so it gets
/// no back-link authored (DESIGN §5, "link target kinds"). A recycle bin written
/// by an older prov may carry one anyway, and re-rendering is not the place to
/// take it away.
fn part_of_of(doc: &Document) -> Option<String> {
    doc.meta
        .get("part_of")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The log document, rendered whole — it is a machine file, laid out by prov
/// rather than edited by hand.
fn render_log(
    title: &str,
    part_of: Option<&str>,
    records: Vec<Value>,
    format: fig::Format,
) -> Result<String> {
    let mut map = prov_graph::meta::Mapping::new();
    map.insert("title".into(), Value::String(title.to_string()));
    if let Some(part_of) = part_of {
        map.insert("part_of".into(), Value::String(part_of.to_string()));
    }
    map.insert("deleted".into(), Value::Sequence(records));
    prov_graph::meta::serialize_mapping(&map, format)
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::delete::Diagnosis;
    use super::super::support::*;
    use super::*;
    use crate::validate::Finding;
    use prov_graph::graph::LinkSite;

    /// A root, a note under it, and the note's bytes — the starting point most
    /// of these need, and the bytes so a test can play the part of the
    /// version-control tool that hands them back.
    fn a_note(tag: &str) -> (PathBuf, &'static str) {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- note.md\n---\n",
        );
        let original = "---\ntitle: My Note\npart_of: index.md\n---\nbody text\n";
        write(&dir, "note.md", original);
        (dir, original)
    }

    #[test]
    fn a_delete_destroys_the_file_and_records_what_it_destroyed() {
        let (dir, _) = a_note("delete-records");

        let danglers = block_on(ws(&dir).delete_with(
            Path::new("note.md"),
            false,
            Some("2026-07-16T10:00:00Z"),
            Diagnosis::Report,
        ))
        .unwrap();
        assert!(danglers.is_empty(), "{danglers:?}");

        // The bytes are gone. This is the whole difference from the bin: prov
        // kept nothing, and says so by keeping nothing.
        assert!(!dir.join("note.md").exists());
        assert!(!dir.join("recyclebin").exists(), "nothing is parked");

        // The parent no longer links it, and the root now links the log.
        let index = read(&dir, "index.md");
        assert!(
            !index.contains("- note.md"),
            "parent entry removed: {index}"
        );
        assert!(index.contains("deletions"), "root links the log: {index}");

        // The record is what `restore` will need, and what a person reading the
        // log wants: where it sat, what it was called, when it went.
        let log = read(&dir, "deletions/index.yaml");
        assert!(log.contains("My Note"), "records the title: {log}");
        assert!(log.contains("note.md"), "records the origin: {log}");
        assert!(log.contains("index.md"), "records the parent: {log}");
        assert!(log.contains("2026-07-16T10:00:00Z"), "records when: {log}");

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(
            findings.is_empty(),
            "a delete leaves check clean: {findings:?}"
        );
    }

    #[test]
    fn a_workspace_that_records_nothing_writes_no_log() {
        // `record_deletions: false` is for a workspace that wants a deletion to
        // leave no trace. It must leave none — not an empty log, not a pointer.
        let (dir, _) = a_note("delete-unrecorded");

        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .record_deletions(false)
            .build();
        block_on(w.delete(Path::new("note.md"), false)).unwrap();

        assert!(!dir.join("note.md").exists());
        assert!(!dir.join("deletions").exists(), "no log was written");
        let index = read(&dir, "index.md");
        assert!(!index.contains("deletions"), "no pointer authored: {index}");
    }

    #[test]
    fn the_bytes_come_back_from_elsewhere_and_restore_puts_the_graph_around_them() {
        // The whole shape of the feature in one test. prov destroys the file and
        // records it; something else — git, a backup, this `write` — returns the
        // bytes; `restore` does the half that has no other owner.
        let (dir, original) = a_note("restore-roundtrip");

        block_on(ws(&dir).delete(Path::new("note.md"), false)).unwrap();
        assert!(!dir.join("note.md").exists());

        write(&dir, "note.md", original);
        block_on(ws(&dir).restore(Path::new("note.md"), Path::new("index.md"))).unwrap();

        // Byte-identical, because prov never touched them.
        assert_eq!(read(&dir, "note.md"), original);
        let index = read(&dir, "index.md");
        assert!(index.contains("note.md"), "parent re-links it: {index}");
        let log = read(&dir, "deletions/index.yaml");
        assert!(!log.contains("My Note"), "the record is spent: {log}");

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(
            findings.is_empty(),
            "a restore leaves check clean: {findings:?}"
        );
    }

    #[test]
    fn restore_refuses_when_the_bytes_are_not_back_and_says_what_to_do() {
        // The one refusal that is new, and the one a user will meet most: the
        // old verb moved bytes home, so "restore" on its own used to be enough.
        // A bare failure to find a file would not tell them what changed.
        let (dir, _) = a_note("restore-no-bytes");
        block_on(ws(&dir).delete(Path::new("note.md"), false)).unwrap();

        let err =
            block_on(ws(&dir).restore(Path::new("note.md"), Path::new("index.md"))).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("nothing is at note.md"), "{text}");
        assert!(text.contains("put the file back first"), "{text}");

        // Refused whole: the record survives, so the restore is still available
        // once the bytes are.
        assert!(read(&dir, "deletions/index.yaml").contains("My Note"));
    }

    #[test]
    fn restore_refuses_a_file_whose_own_id_is_not_the_records() {
        // The bytes come back from outside prov, so what lands at the path is
        // whatever the caller put there. A file spelling a different id is not
        // the document this record is about, and re-registering the record's id
        // onto it would leave two documents claiming one id.
        let dir = tempdir("restore-wrong-file");
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
        block_on(w.delete(Path::new("note.md"), false)).unwrap();

        // The wrong file put back at the right path.
        write(
            &dir,
            "note.md",
            "---\ntitle: Something Else\npart_of: index.md\nid: zzzzzzz\n---\n",
        );
        let err = block_on(w.restore(Path::new("note.md"), Path::new("index.md"))).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("carries id zzzzzzz"), "{text}");
        assert!(
            text.contains("b7k2m"),
            "names the id the record expects: {text}"
        );
    }

    #[test]
    fn restore_refuses_to_take_an_id_from_the_document_that_now_holds_it() {
        // The record carries the id, and that id has been out of the registry
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
        let original = "---\ntitle: My Note\npart_of: index.md\nid: b7k2m\n---\nbody\n";
        write(&dir, "note.md", original);
        write(
            &dir,
            "other.md",
            "---\ntitle: Other\npart_of: index.md\nid: b7k2m\n---\n",
        );

        let mut w = id_ws(&dir);
        let id = prov_graph::identity::Id("b7k2m".into());
        w.index_mut().register(&id, Path::new("note.md"));
        block_on(w.delete(Path::new("note.md"), false)).unwrap();
        // The arrival: while the record sat in the log, the id turned up elsewhere.
        w.index_mut().register(&id, Path::new("other.md"));
        write(&dir, "note.md", original);

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
        // Refused up front: the parent was not touched.
        assert!(!read(&dir, "index.md").contains("- note.md"));

        // Precise, not blanket: once nothing else claims the id, it restores.
        w.index_mut().unregister(&id);
        block_on(w.restore(Path::new("note.md"), Path::new("index.md"))).unwrap();
        assert!(read(&dir, "index.md").contains("note.md"));
    }

    #[test]
    fn restore_refuses_when_another_id_already_claims_the_path() {
        // The other direction: the file is back and spells nothing, but the
        // registry binds that path to a different id. Restoring would drop that
        // id out of the registry — a live id silently demoted.
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
        block_on(w.delete(Path::new("note.md"), false)).unwrap();
        w.index_mut().register(
            &prov_graph::identity::Id("zzzzzzz".into()),
            Path::new("note.md"),
        );
        // Back without an `id:` of its own, so the record's id is the only
        // candidate and the path binding is what refuses.
        write(
            &dir,
            "note.md",
            "---\ntitle: My Note\npart_of: index.md\n---\n",
        );

        let err = block_on(w.restore(Path::new("note.md"), Path::new("index.md"))).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Collision(prov_graph::index::Collision::Path { .. })
            ),
            "{err:?}"
        );
        assert!(
            !read(&dir, "index.md").contains("- note.md"),
            "nothing relinked"
        );
    }

    #[test]
    fn a_second_deletion_appends_and_the_pointer_is_authored_once() {
        let dir = tempdir("delete-append");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n- b.md\n---\n");
        write(&dir, "a.md", "---\ntitle: Aye\npart_of: index.md\n---\n");
        write(&dir, "b.md", "---\ntitle: Bee\npart_of: index.md\n---\n");

        block_on(ws(&dir).delete(Path::new("a.md"), false)).unwrap();
        block_on(ws(&dir).delete(Path::new("b.md"), false)).unwrap();

        let log = read(&dir, "deletions/index.yaml");
        assert!(
            log.contains("Aye") && log.contains("Bee"),
            "both recorded: {log}"
        );

        let index = read(&dir, "index.md");
        assert_eq!(
            index.matches("deletions:").count(),
            1,
            "pointer authored once: {index}"
        );

        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn deleting_a_parentless_document_records_it_against_the_real_root() {
        // Orphans are precisely what a user deletes: `check` reports them as the
        // onboarding signal, and the answer is often "this was junk". The walk up
        // from one finds no parent, so it falls back to the *discovered* root —
        // which is a real other document, and the right place for the pointer.
        let dir = tempdir("delete-parentless");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(&dir, "loose.md", "---\ntitle: Loose\n---\nno parent\n");

        block_on(ws(&dir).delete(Path::new("loose.md"), false)).unwrap();

        assert!(!dir.join("loose.md").exists(), "gone, and it stays gone");
        assert!(read(&dir, "deletions/index.yaml").contains("Loose"));
        let index = read(&dir, "index.md");
        assert!(
            index.contains("deletions:"),
            "linked from the root: {index}"
        );
    }

    #[test]
    fn deleting_the_document_that_is_the_root_does_not_resurrect_it() {
        // The case that fallback cannot save: the walk lands on the subject
        // because the subject *is* the root. Writing the pointer there stages a
        // write to the path this same change set is removing, which recreates the
        // file — the user sees the document still sitting there, now carrying a
        // pointer it never had, and believes the delete failed.
        //
        // There is no reachable root to link from, so the log is written unlinked
        // and the next delete from a real node adopts it.
        let dir = tempdir("delete-is-root");
        write(&dir, "solo.md", "---\ntitle: Solo\n---\nthe only one\n");

        block_on(ws(&dir).delete(Path::new("solo.md"), false)).unwrap();

        assert!(!dir.join("solo.md").exists(), "gone, and it stays gone");
        assert!(read(&dir, "deletions/index.yaml").contains("Solo"));
    }

    #[test]
    fn an_unlinked_log_is_adopted_rather_than_collided_with() {
        // The follow-on. A log left unlinked by the case above is still on disk,
        // and the next delete plans the very same path for it. Discovering it
        // only through the root's pointer would compute an empty record list,
        // expect the file to be absent, and refuse every later delete.
        let dir = tempdir("delete-adopt");
        write(&dir, "solo.md", "---\ntitle: Solo\n---\n");
        block_on(ws(&dir).delete(Path::new("solo.md"), false)).unwrap();
        assert!(read(&dir, "deletions/index.yaml").contains("Solo"));

        // Now a real tree beside it, and a delete within that.
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- a.md\n---\n",
        );
        write(&dir, "a.md", "---\ntitle: Aye\npart_of: index.md\n---\n");
        block_on(ws(&dir).delete(Path::new("a.md"), false)).unwrap();

        let log = read(&dir, "deletions/index.yaml");
        assert!(
            log.contains("Solo") && log.contains("Aye"),
            "both kept: {log}"
        );
        assert!(
            read(&dir, "index.md").contains("deletions:"),
            "and now linked"
        );
    }

    #[test]
    fn clear_deletions_forgets_the_records_but_keeps_the_log() {
        let dir = tempdir("clear-deletions");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\ntitle: Aye\npart_of: index.md\n---\n");

        block_on(ws(&dir).delete(Path::new("a.md"), false)).unwrap();
        assert_eq!(
            block_on(ws(&dir).clear_deletions(Path::new("index.md"))).unwrap(),
            1
        );

        let log = read(&dir, "deletions/index.yaml");
        assert!(!log.contains("Aye"), "records cleared: {log}");
        // The member itself survives, still linked and consistent.
        assert!(read(&dir, "index.md").contains("deletions"));
        let findings = block_on(ws(&dir).check(Path::new("index.md"))).unwrap();
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn delete_refuses_a_separated_body_and_names_its_node() {
        let dir = tempdir("delete-separated-body");
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

        let err = block_on(ws(&dir).delete(Path::new("b.md"), false)).unwrap_err();
        assert!(err.to_string().contains("is the body of b.yaml"), "{err}");
        assert!(dir.join("b.md").exists(), "nothing was destroyed");

        // Forced, it goes and the stranded pointer is reported.
        let danglers = block_on(ws(&dir).delete(Path::new("b.md"), true)).unwrap();
        assert!(!dir.join("b.md").exists());
        assert!(
            danglers.iter().any(|f| matches!(f,
                Finding::BrokenLink { doc, site: LinkSite::Relation(r), target }
                    if doc == &PathBuf::from("b.yaml") && r == "content" && target == "b.md")),
            "{danglers:?}"
        );
    }

    #[test]
    fn a_skipped_diagnosis_still_records_everything_restore_needs() {
        // The record and the parent edit are what `restore` reads, so a delete
        // that quietly did less of either would be a document the user could not
        // put back — the one failure a "faster delete" must not be able to cause.
        let dir = tempdir("delete-skip");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- note.md\n- sub/linker.md\n---\n",
        );
        let original = "---\ntitle: Note\npart_of: index.md\n---\nbody\n";
        write(&dir, "note.md", original);
        write(
            &dir,
            "sub/linker.md",
            "---\npart_of: /index.md\nlinks:\n- /note.md\n---\n",
        );

        let fs = crate::fs_faults::CountingFs::default();
        let mut workspace = Workspace::builder(fs.clone()).root(&dir).build();
        let danglers = block_on(workspace.delete_with(
            Path::new("note.md"),
            false,
            Some("2026-08-18T00:00:00Z"),
            Diagnosis::Skip,
        ))
        .unwrap();

        assert!(danglers.is_empty(), "{danglers:?}");
        assert_eq!(
            fs.doc_reads(&dir, "sub/linker.md"),
            0,
            "a skipped diagnosis still censused the workspace"
        );
        assert!(
            !read(&dir, "index.md").contains("note.md"),
            "parent entry removed"
        );

        // And back again, from the record the skipped delete still wrote.
        write(&dir, "note.md", original);
        block_on(workspace.restore(Path::new("note.md"), Path::new("index.md"))).unwrap();
        assert!(read(&dir, "index.md").contains("note.md"), "re-linked");
    }

    #[test]
    fn a_failed_delete_leaves_the_workspace_untouched() {
        // The removal, the parent edit and the record are one journaled
        // ChangeSet, so an I/O failure part-way rolls back to exactly the
        // starting state — never a file destroyed with nothing written down.
        let (dir, _) = a_note("delete-atomic");
        let before = snapshot(&dir);

        let mut w = Workspace::builder(FailAtWrite::nth(0)).root(&dir).build();
        let err = block_on(w.delete(Path::new("note.md"), false)).unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");

        assert_eq!(snapshot(&dir), before, "a failed delete tore the workspace");
    }

    #[test]
    fn a_legacy_bin_record_still_restores_by_moving_its_parked_bytes_home() {
        // The migration path. A workspace that binned documents before the log
        // replaced it has their only copy under `items/`, reachable through the
        // old pointer — so `restore` has to keep moving those home, or the rename
        // would strand them.
        let dir = tempdir("legacy-restore");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\nrecycle_bin: recyclebin/index.yaml\n---\n",
        );
        write(
            &dir,
            "recyclebin/index.yaml",
            "title: Recycle Bin\ndeleted:\n- from: note.md\n  title: My Note\n  bin: recyclebin/items/note.md\n  parent: index.md\n",
        );
        let original = "---\ntitle: My Note\npart_of: index.md\n---\nbody\n";
        write(&dir, "recyclebin/items/note.md", original);

        block_on(ws(&dir).restore(Path::new("note.md"), Path::new("index.md"))).unwrap();

        assert_eq!(
            read(&dir, "note.md"),
            original,
            "the parked bytes came home"
        );
        assert!(!dir.join("recyclebin/items/note.md").exists());
        assert!(
            read(&dir, "index.md").contains("note.md"),
            "and it is relinked"
        );
        assert!(!read(&dir, "recyclebin/index.yaml").contains("My Note"));
    }

    #[test]
    fn a_legacy_bin_is_emptied_by_clearing_its_deletions() {
        // `empty-bin`'s job, under the verb that replaced it: an unmigrated bin
        // has bytes to destroy, and forgetting the records without them would
        // leave the bytes orphaned under `items/` with nothing naming them.
        let dir = tempdir("legacy-clear");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\nrecycle_bin: recyclebin/index.yaml\n---\n",
        );
        write(
            &dir,
            "recyclebin/index.yaml",
            "title: Recycle Bin\ndeleted:\n- from: note.md\n  title: My Note\n  bin: recyclebin/items/note.md\n",
        );
        write(
            &dir,
            "recyclebin/items/note.md",
            "---\ntitle: My Note\n---\n",
        );

        assert_eq!(
            block_on(ws(&dir).clear_deletions(Path::new("index.md"))).unwrap(),
            1
        );
        assert!(
            !dir.join("recyclebin/items/note.md").exists(),
            "bytes purged"
        );
        assert!(!read(&dir, "recyclebin/index.yaml").contains("My Note"));
    }
}
