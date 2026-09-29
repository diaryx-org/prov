//! Which documents an export's hold keeps back because of the *term* they
//! carry — the half of the hold only a workspace can answer.
//!
//! An export's `hold` names a field. A document declaring the literal `true`
//! there waits, and `prov-exports` judges that alone. A document whose value
//! there is a term its vocabulary marks `holds: true` waits too — `hold:
//! status` keeps back a `status: draft` proposal while `status: accepted`
//! leaves — and that needs the vocabulary, which needs the declaration that
//! governs the document, which needs the tree: `status` means one set of terms
//! under `Tasks` and another under `Proposals`. [`Workspace::term_holds`] asks
//! all three and hands `prov-exports` the answer as a set of paths.
//!
//! [`Workspace::export_plan`] is the whole plan with that answer in it, and the
//! one a consumer should call: `prov_exports::plan` takes the answer as a
//! required argument precisely so that a caller who never asked cannot let
//! every draft leave.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::Path;

use prov_exports::{ExportPlan, ExportSpec, TermHolds};
use prov_graph::error::Result;
use prov_graph::fs::ReadStorage;
use prov_graph::index::IdIndex;
use prov_views::ViewSpec;

use crate::vocabulary::Vocabulary;
use crate::workspace::Workspace;

impl<FS: ReadStorage, Id, Ix: IdIndex> Workspace<FS, Id, Ix> {
    /// The documents reachable from `root_doc` that `spec`'s hold keeps back by
    /// the term they carry: each document's value under the hold field, read
    /// against the vocabulary of the declaration that governs *that* document,
    /// names a term that `holds: true`.
    ///
    /// Empty for an export with no hold, and for a hold field that declares no
    /// vocabulary — such a field holds by the literal `true` alone. A store
    /// that will not load holds nothing back by term: its own finding is
    /// `check`'s, and the literal `true` still holds.
    pub async fn term_holds(&self, root_doc: &Path, spec: &ExportSpec) -> Result<TermHolds> {
        let Some(field) = spec.hold.as_deref() else {
            return Ok(TermHolds::none());
        };
        let config = self.effective_config(root_doc).await?;
        let Some(declarations) = config.fields.get(field) else {
            return Ok(TermHolds::none());
        };
        if declarations.iter().all(|d| d.vocabulary.is_none()) {
            return Ok(TermHolds::none());
        }
        let scopes = self.field_scopes_of(root_doc, &config).await?;
        let rows = prov_views::documents(self.graph(), root_doc)
            .await
            .map_err(|prov_views::Error::Graph(error)| error)?;
        // Each declaration's vocabulary, loaded the first time a document it
        // governs is met, since a scoped field has one per scope.
        let mut vocabularies: HashMap<usize, Option<Vocabulary>> = HashMap::new();
        let mut held = Vec::new();
        for row in rows {
            let Some(value) = row.meta.get(field) else {
                continue;
            };
            let Some(index) = scopes.index_for(&config, field, &row.path) else {
                continue;
            };
            if let Entry::Vacant(slot) = vocabularies.entry(index) {
                slot.insert(
                    self.load_field_vocabulary(root_doc, field, &declarations[index])
                        .await
                        .ok()
                        .flatten(),
                );
            }
            if let Some(Some(vocabulary)) = vocabularies.get(&index)
                && prov_grain::scalar_texts(value)
                    .iter()
                    .any(|term| vocabulary.holds(term.trim()))
            {
                held.push(row.path);
            }
        }
        Ok(TermHolds::from_docs(held))
    }

    /// What `spec` lets leave the workspace at `root_doc` — the export's plan,
    /// with its hold read through the workspace's vocabularies as well as the
    /// literal `true` ([`term_holds`](Self::term_holds)).
    pub async fn export_plan(
        &self,
        root_doc: &Path,
        spec: &ExportSpec,
        views: &[ViewSpec],
    ) -> prov_exports::Result<ExportPlan> {
        let term_holds = self.term_holds(root_doc, spec).await?;
        prov_exports::plan(self.graph(), spec, views, root_doc, &term_holds).await
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use std::path::PathBuf;

    use prov_exports::Gate;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_testkit::write;

    use super::*;

    /// A public tasks corner and a public proposals corner, each with its own
    /// `status` vocabulary, and one word — `draft` — in both: a proposal's
    /// draft holds, a task's does not.
    fn workspace(tag: &str) -> PathBuf {
        let dir = prov_testkit::scratch("term-holds", tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nconfig: prov.yaml\ncontents:\n- tasks.md\n- proposals.md\n---\n",
        );
        write(
            &dir,
            "prov.yaml",
            "title: prov config\nfields:\n  status:\n    - under: '[[Tasks]]'\n      values: closed\n      vocabulary: /task-statuses.yaml\n    - under: '[[Proposals]]'\n      values: closed\n      vocabulary: /proposal-statuses.yaml\nexports:\n  www:\n    gate: { field: audience, value: public }\n    hold: status\n",
        );
        write(
            &dir,
            "task-statuses.yaml",
            "title: Task statuses\nvocabulary: { field: status, values: closed }\nterms:\n  draft: {}\n  open: {}\n",
        );
        write(
            &dir,
            "proposal-statuses.yaml",
            "title: Proposal statuses\nvocabulary: { field: status, values: closed }\nterms:\n  draft: { holds: true }\n  accepted: {}\n",
        );
        write(
            &dir,
            "tasks.md",
            "---\ntitle: Tasks\npart_of: index.md\ncontents:\n- sketch.md\n---\n",
        );
        write(
            &dir,
            "sketch.md",
            "---\ntitle: Sketch\npart_of: tasks.md\naudience: public\nstatus: draft\n---\n",
        );
        write(
            &dir,
            "proposals.md",
            "---\ntitle: Proposals\npart_of: index.md\ncontents:\n- idea.md\n- settled.md\n---\n",
        );
        write(
            &dir,
            "idea.md",
            "---\ntitle: Idea\npart_of: proposals.md\naudience: public\nstatus: draft\n---\n",
        );
        write(
            &dir,
            "settled.md",
            "---\ntitle: Settled\npart_of: proposals.md\naudience: public\nstatus: accepted\n---\n",
        );
        dir
    }

    fn www(hold: Option<&str>) -> ExportSpec {
        ExportSpec {
            name: "www".into(),
            label: None,
            gate: Gate {
                field: "audience".into(),
                value: "public".into(),
            },
            hold: hold.map(str::to_string),
            view: None,
        }
    }

    fn paths(docs: &[prov_exports::ExportDoc]) -> Vec<&str> {
        docs.iter().map(|d| d.path.to_str().unwrap()).collect()
    }

    #[test]
    fn a_term_holds_under_the_vocabulary_that_governs_the_document() {
        let dir = workspace("scoped");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let root = Path::new("index.md");

        let plan = block_on(ws.export_plan(root, &www(Some("status")), &[])).unwrap();
        assert_eq!(paths(&plan.held), ["idea.md"]);
        assert_eq!(paths(&plan.entries), ["settled.md", "sketch.md"]);

        // No hold, no term read: everything the gate admits leaves.
        let plan = block_on(ws.export_plan(root, &www(None), &[])).unwrap();
        assert!(plan.held.is_empty());
        assert_eq!(plan.entries.len(), 3);
    }
}
