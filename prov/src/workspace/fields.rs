//! Which declaration of a field governs a document — the *where* a scoped
//! `fields` entry asks before the *what*.
//!
//! A field may be declared once for the whole workspace, or several times,
//! each under an index: `status` is one closed set of terms under `Tasks` and
//! another under `Proposals`, and nothing at all elsewhere. The declaration is
//! a property of a region of the tree, so every reader of `fields` that holds
//! a document — `check` judging a term, `new` writing a starting value, a
//! repair widening a vocabulary — has to ask which region the document is in
//! first. [`FieldScopes`] is that answer, computed once per walk: each scoped
//! declaration's `under` is resolved exactly as a view's anchor is (a path, an
//! `id:`, or a title) and its subtree collected, and a document is governed by
//! the deepest scope that contains it, or by the unscoped declaration when
//! none does.
//!
//! The index is not in its own scope. That is the same rule a view keeps for
//! its anchor — an index is what records hang *under*, not one of them — and
//! it is what keeps a `Tasks` index from opening as `status: open`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use prov_graph::error::Result;
use prov_graph::fs::ReadStorage;
use prov_graph::graph::{NodeKind, Target, TreeOptions};
use prov_graph::index::IdIndex;
use prov_graph::link::{self, Link};
use prov_graph::meta::{Mapping, Value};
use prov_graph::title::{self, TitleIndex};

use crate::config::{FieldSpec, WorkspaceConfig};
use crate::workspace::Workspace;

/// One scoped declaration, resolved: the index it hangs under and every
/// document below it.
#[derive(Debug, Clone)]
struct Scope {
    field: String,
    /// The declaration's position in the field's list.
    index: usize,
    anchor: PathBuf,
    members: BTreeSet<PathBuf>,
}

/// A scoped declaration whose `under` named nothing prov could walk to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    /// The field.
    pub field: String,
    /// The declaration's position in the field's list.
    pub index: usize,
    /// The anchor as written.
    pub under: String,
    /// Why, in a sentence a user can act on.
    pub why: String,
}

/// The scopes of every scoped field declaration in a workspace, resolved
/// against its spanning tree. Built by [`Workspace::field_scopes`].
#[derive(Debug, Clone, Default)]
pub struct FieldScopes {
    scopes: Vec<Scope>,
    unresolved: Vec<Unresolved>,
}

impl FieldScopes {
    /// The position, in `field`'s declaration list, of the declaration that
    /// governs `doc`: the deepest scope containing it, else the unscoped one.
    /// `None` when the field is not declared for this document at all.
    pub fn index_for(&self, config: &WorkspaceConfig, field: &str, doc: &Path) -> Option<usize> {
        let doc = link::normalize(doc);
        self.governing(config, field, |scope| scope.members.contains(&doc))
    }

    /// The declaration of `field` that governs `doc`. See
    /// [`index_for`](Self::index_for).
    pub fn spec_for<'c>(
        &self,
        config: &'c WorkspaceConfig,
        field: &str,
        doc: &Path,
    ) -> Option<&'c FieldSpec> {
        let index = self.index_for(config, field, doc)?;
        config.fields.get(field)?.get(index)
    }

    /// The declaration of `field` that will govern a document *about to be
    /// made* as a spanning child of `parent`: the child is in every scope the
    /// parent is in, and in the parent's own scope when the parent is an
    /// index.
    pub fn spec_for_child<'c>(
        &self,
        config: &'c WorkspaceConfig,
        field: &str,
        parent: &Path,
    ) -> Option<&'c FieldSpec> {
        let parent = link::normalize(parent);
        let index = self.governing(config, field, |scope| {
            scope.anchor == parent || scope.members.contains(&parent)
        })?;
        config.fields.get(field)?.get(index)
    }

    /// The starting values a document made under `parent` opens with — each
    /// field's governing declaration's `default`, in field order.
    pub fn defaults_for_child(&self, config: &WorkspaceConfig, parent: &Path) -> Mapping {
        let mut out = Mapping::new();
        for field in config.fields.keys() {
            if let Some(spec) = self.spec_for_child(config, field, parent)
                && let Some(default) = &spec.default
            {
                out.insert(field.clone(), default.clone());
            }
        }
        out
    }

    /// The scoped declarations whose anchor resolved to nothing — each
    /// governs no document, and a reader may want to say so.
    pub fn unresolved(&self) -> &[Unresolved] {
        &self.unresolved
    }

    /// The governing declaration's index under a containment test: the
    /// smallest containing scope wins, because where scopes nest the deeper
    /// one's subtree is the smaller.
    fn governing(
        &self,
        config: &WorkspaceConfig,
        field: &str,
        contains: impl Fn(&Scope) -> bool,
    ) -> Option<usize> {
        let declarations = config.fields.get(field)?;
        let mut best: Option<(usize, usize)> = None;
        for scope in self.scopes.iter().filter(|s| s.field == field) {
            if contains(scope) && best.is_none_or(|(size, _)| scope.members.len() < size) {
                best = Some((scope.members.len(), scope.index));
            }
        }
        match best {
            Some((_, index)) => Some(index),
            None => declarations.iter().position(|d| d.under.is_none()),
        }
    }
}

impl<FS: ReadStorage, Id, Ix: IdIndex> Workspace<FS, Id, Ix> {
    /// Resolve every scoped field declaration against the tree below
    /// `root_doc`. One walk per scoped declaration; a workspace whose fields
    /// are all unscoped walks nothing and gets an empty, always-fallback
    /// answer.
    pub async fn field_scopes(&self, root_doc: &Path) -> Result<FieldScopes> {
        let config = self.effective_config(root_doc).await?;
        self.field_scopes_of(root_doc, &config).await
    }

    /// [`field_scopes`](Self::field_scopes) for a config the caller already
    /// holds.
    pub async fn field_scopes_of(
        &self,
        root_doc: &Path,
        config: &WorkspaceConfig,
    ) -> Result<FieldScopes> {
        let root_doc = link::normalize(root_doc);
        let mut out = FieldScopes::default();
        // The title index costs a scan, so it is built once, and only if some
        // anchor is a name.
        let mut titles: Option<TitleIndex> = None;
        for (field, declarations) in &config.fields {
            for (index, spec) in declarations.iter().enumerate() {
                let Some(under) = &spec.under else { continue };
                let link = Link::parse(under);
                let nominal = !link.is_external()
                    && !link.is_same_document()
                    && link.id_ref().is_none()
                    && title::is_alias_shaped(link.addressed_target());
                if nominal && titles.is_none() {
                    titles = Some(self.title_index_scoped(&root_doc).await?);
                }
                let unresolved = |why: String| Unresolved {
                    field: field.clone(),
                    index,
                    under: under.clone(),
                    why,
                };
                let anchor = match self.resolve_link_with(&root_doc, &link, titles.as_ref()) {
                    Target::Path(path) => path,
                    Target::UnresolvedId(id) => {
                        out.unresolved.push(unresolved(format!(
                            "no document is registered under the id `{}`",
                            id.0
                        )));
                        continue;
                    }
                    Target::AmbiguousAlias(name) => {
                        out.unresolved
                            .push(unresolved(format!("several documents are titled `{name}`")));
                        continue;
                    }
                    Target::External | Target::SameDocument | Target::Foreign { .. } => {
                        out.unresolved.push(unresolved(
                            "an anchor must name a document in this workspace".to_string(),
                        ));
                        continue;
                    }
                };
                let tree = self
                    .graph
                    .tree_with(
                        &anchor,
                        TreeOptions {
                            ignore_missing: true,
                        },
                    )
                    .await?;
                if !matches!(tree.kind, NodeKind::Doc) {
                    out.unresolved
                        .push(unresolved("no document exists there".to_string()));
                    continue;
                }
                let mut members = BTreeSet::new();
                for child in &tree.children {
                    collect(child, &mut members);
                }
                out.scopes.push(Scope {
                    field: field.clone(),
                    index,
                    anchor,
                    members,
                });
            }
        }
        Ok(out)
    }
}

/// One document on the way up from a parent to the root, with the names a
/// title anchor could match it by — its `title`, and its file stem, the two
/// spellings the title index registers a document under.
struct Rung {
    path: PathBuf,
    names: Vec<String>,
}

impl<FS: ReadStorage, Id, Ix: IdIndex> Workspace<FS, Id, Ix> {
    /// The declaration of each field that will govern a document about to be
    /// made as a spanning child of `parent` — the answer
    /// [`FieldScopes::spec_for_child`] gives, reached by climbing rather than
    /// scanning.
    ///
    /// [`field_scopes`](Self::field_scopes) places every anchor and collects
    /// every subtree, which is the right shape for `check` — it holds every
    /// document and asks about each — and the wrong one for `new`, which holds
    /// one parent and asks once: a title anchor costs a scan of the whole
    /// workspace to place, and the subtree walks cost the rest of it. But the
    /// child is in exactly the scopes whose anchor is `parent` or one of its
    /// ancestors, and those are the documents on the way up the spanning
    /// inverse from `parent` — a read per level of depth, whatever the
    /// workspace's size. The deepest anchor on the way up is the smallest
    /// containing scope, so it governs, as `governing` decides; a field with
    /// no anchor on the way up falls back to its unscoped declaration, if it
    /// has one. A workspace whose declarations are all unscoped climbs nothing.
    ///
    /// The one answer that differs: a title anchor that several documents
    /// carry. The scan sees the ambiguity and lets the declaration govern
    /// nothing; the climb sees only the ancestor carrying the name, and lets it
    /// govern. `check` reports the ambiguity either way.
    pub async fn field_specs_for_child<'c>(
        &self,
        root_doc: &Path,
        config: &'c WorkspaceConfig,
        parent: &Path,
    ) -> Result<BTreeMap<String, &'c FieldSpec>> {
        let scoped = config
            .fields
            .values()
            .flatten()
            .any(|spec| spec.under.is_some());
        let lineage = if scoped {
            self.lineage(parent).await?
        } else {
            Vec::new()
        };
        let root_doc = link::normalize(root_doc);
        let mut out = BTreeMap::new();
        for (field, declarations) in &config.fields {
            // The nearest anchor wins: walk the rungs from the parent up, and the
            // first declaration anchored at one is the deepest scope the child
            // is in.
            let scoped = lineage.iter().find_map(|rung| {
                declarations
                    .iter()
                    .find(|spec| self.anchored_at(&root_doc, spec, rung))
            });
            let governing = scoped.or_else(|| declarations.iter().find(|d| d.under.is_none()));
            if let Some(spec) = governing {
                out.insert(field.clone(), spec);
            }
        }
        Ok(out)
    }

    /// The starting values a document made under `parent` opens with — each
    /// field's governing declaration's `default`, in field order.
    /// [`FieldScopes::defaults_for_child`] by way of
    /// [`field_specs_for_child`](Self::field_specs_for_child).
    pub async fn defaults_for_child(
        &self,
        root_doc: &Path,
        config: &WorkspaceConfig,
        parent: &Path,
    ) -> Result<Mapping> {
        let mut out = Mapping::new();
        for (field, spec) in self.field_specs_for_child(root_doc, config, parent).await? {
            if let Some(default) = &spec.default {
                out.insert(field, default.clone());
            }
        }
        Ok(out)
    }

    /// Whether `spec`'s anchor names the document at `rung`: by path or `id:`
    /// through the ordinary link resolution, by title against the names the
    /// rung carries. An unscoped declaration is anchored nowhere.
    fn anchored_at(&self, root_doc: &Path, spec: &FieldSpec, rung: &Rung) -> bool {
        let Some(under) = &spec.under else {
            return false;
        };
        let link = Link::parse(under);
        if link.is_external() || link.is_same_document() {
            return false;
        }
        let addressed = link.addressed_target();
        if link.id_ref().is_none() && title::is_alias_shaped(addressed) {
            return rung.names.iter().any(|name| name == addressed);
        }
        self.resolve_link_with(root_doc, &link, None) == Target::Path(rung.path.clone())
    }

    /// `from` and its ancestors up the spanning inverse, nearest first — the
    /// documents whose scope a child of `from` would be in. Stops at the
    /// document nothing contains, at a cycle, or at an ancestor that cannot be
    /// read; a first target that is not a document in this workspace ends the
    /// climb the same way. The spanning relation is where the config puts it;
    /// a workspace without one has no containment to climb.
    async fn lineage(&self, from: &Path) -> Result<Vec<Rung>> {
        let relations = self.relations();
        let inverse = relations
            .spanning_relation()
            .and_then(|spanning| relations.relations().iter().find(|r| r.name == spanning))
            .and_then(|r| r.inverse.clone());
        let mut rungs = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = link::normalize(from);
        while seen.insert(current.clone()) {
            let Ok((_, doc)) = self.load(&current).await else {
                break;
            };
            let mut names = Vec::new();
            if let Some(stem) = current.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
            if let Some(title) = doc.meta.get("title").and_then(Value::as_str) {
                names.push(title.to_string());
            }
            let up = inverse.as_deref().and_then(|inverse| {
                let raw = doc
                    .meta
                    .get(inverse)
                    .map(Value::link_strings)?
                    .into_iter()
                    .next()?;
                match self.resolve_link(&current, &Link::parse(&raw)) {
                    Target::Path(p) => Some(p),
                    _ => None,
                }
            });
            rungs.push(Rung {
                path: current.clone(),
                names,
            });
            match up {
                Some(parent) => current = parent,
                None => break,
            }
        }
        Ok(rungs)
    }
}

/// Every readable document in a subtree, the anchor excluded by the caller.
fn collect(node: &prov_graph::graph::Node, out: &mut BTreeSet<PathBuf>) {
    if matches!(node.kind, NodeKind::Doc) {
        out.insert(node.path.clone());
    }
    for child in &node.children {
        collect(child, out);
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::config::OpenClosed;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_testkit::write;

    /// A docs workspace with a tasks corner and a proposals corner, and a
    /// `status` declared differently under each — plus an unscoped `audience`.
    fn workspace(tag: &str) -> PathBuf {
        let dir = prov_testkit::scratch("field-scopes", tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nconfig: prov.yaml\ncontents:\n- docs/tasks/tasks.md\n- docs/proposals/proposals.md\n- readme.md\n---\n",
        );
        write(
            &dir,
            "prov.yaml",
            "title: prov config\nfields:\n  status:\n    - under: '[[Tasks]]'\n      values: closed\n      vocabulary: '[T](/vocab/t.yaml)'\n      default: open\n    - under: '[[Proposals]]'\n      values: closed\n      vocabulary: '[P](/vocab/p.yaml)'\n      default: draft\n  audience:\n    default: public\n",
        );
        write(
            &dir,
            "readme.md",
            "---\ntitle: Readme\npart_of: /index.md\n---\n",
        );
        write(
            &dir,
            "docs/tasks/tasks.md",
            "---\ntitle: Tasks\npart_of: /index.md\ncontents:\n- fix.md\n- sub/index.md\n---\n",
        );
        write(
            &dir,
            "docs/tasks/fix.md",
            "---\ntitle: Fix\npart_of: tasks.md\n---\n",
        );
        write(
            &dir,
            "docs/tasks/sub/index.md",
            "---\ntitle: Sub\npart_of: ../tasks.md\ncontents:\n- deep.md\n---\n",
        );
        write(
            &dir,
            "docs/tasks/sub/deep.md",
            "---\ntitle: Deep\npart_of: index.md\n---\n",
        );
        write(
            &dir,
            "docs/proposals/proposals.md",
            "---\ntitle: Proposals\npart_of: /index.md\ncontents:\n- idea.md\n---\n",
        );
        write(
            &dir,
            "docs/proposals/idea.md",
            "---\ntitle: Idea\npart_of: proposals.md\n---\n",
        );
        dir
    }

    #[test]
    fn a_document_is_governed_by_the_scope_it_sits_in_and_the_index_by_none() {
        let dir = workspace("governs");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let config = block_on(ws.effective_config(Path::new("index.md"))).unwrap();
        let scopes = block_on(ws.field_scopes_of(Path::new("index.md"), &config)).unwrap();
        assert!(scopes.unresolved().is_empty(), "{:?}", scopes.unresolved());

        let default = |doc: &str| {
            scopes
                .spec_for(&config, "status", Path::new(doc))
                .and_then(|s| s.default.clone())
        };
        assert_eq!(
            default("docs/tasks/fix.md"),
            Some(Value::String("open".into()))
        );
        assert_eq!(
            default("docs/tasks/sub/deep.md"),
            Some(Value::String("open".into()))
        );
        assert_eq!(
            default("docs/proposals/idea.md"),
            Some(Value::String("draft".into()))
        );
        // The index is not in its own scope, and a document outside both
        // scopes has no `status` declaration at all — but keeps the unscoped
        // `audience`.
        assert_eq!(default("docs/tasks/tasks.md"), None);
        assert_eq!(default("readme.md"), None);
        assert_eq!(
            scopes
                .spec_for(&config, "audience", Path::new("readme.md"))
                .map(|s| s.values),
            Some(OpenClosed::Open)
        );

        // What a child opens with is decided by its parent: under the index
        // itself, or anywhere below it.
        let opening = |parent: &str| scopes.defaults_for_child(&config, Path::new(parent));
        let task = opening("docs/tasks/tasks.md");
        assert_eq!(task.get("status"), Some(&Value::String("open".into())));
        assert_eq!(task.get("audience"), Some(&Value::String("public".into())));
        assert_eq!(
            opening("docs/tasks/sub/index.md").get("status"),
            Some(&Value::String("open".into()))
        );
        assert_eq!(
            opening("docs/proposals/proposals.md").get("status"),
            Some(&Value::String("draft".into()))
        );
        assert_eq!(opening("index.md").get("status"), None);
    }

    /// The climb answers what a child opens with exactly as the scan does —
    /// under an index, anywhere below it, and nowhere — and reads only the
    /// way up: a workspace whose `Tasks` index is unreadable to the scan
    /// (a sibling subtree that cannot be walked) does not stop a child of
    /// `Proposals` from opening as a draft.
    #[test]
    fn a_child_opens_the_same_by_climbing_as_by_scanning() {
        let dir = workspace("climb");
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let config = block_on(ws.effective_config(Path::new("index.md"))).unwrap();
        let scopes = block_on(ws.field_scopes_of(Path::new("index.md"), &config)).unwrap();
        for parent in [
            "docs/tasks/tasks.md",
            "docs/tasks/sub/index.md",
            "docs/tasks/fix.md",
            "docs/proposals/proposals.md",
            "docs/proposals/idea.md",
            "readme.md",
            "index.md",
        ] {
            let scanned = scopes.defaults_for_child(&config, Path::new(parent));
            let climbed =
                block_on(ws.defaults_for_child(Path::new("index.md"), &config, Path::new(parent)))
                    .unwrap();
            assert_eq!(climbed, scanned, "under {parent}");
        }

        // A subtree the scan cannot place — `Tasks` renamed away from what the
        // root's `contents` names — leaves the climb from `Proposals` untouched.
        std::fs::rename(dir.join("docs/tasks"), dir.join("docs/was-tasks")).unwrap();
        let climbed = block_on(ws.defaults_for_child(
            Path::new("index.md"),
            &config,
            Path::new("docs/proposals/proposals.md"),
        ))
        .unwrap();
        assert_eq!(climbed.get("status"), Some(&Value::String("draft".into())));
    }

    /// Nested scopes: the nearest anchor on the way up governs, an anchor by
    /// path or by file stem is matched as the scan matches it, and a field
    /// with no anchor on the way up falls back to its unscoped declaration.
    #[test]
    fn the_climb_takes_the_nearest_anchor_by_title_stem_or_path() {
        let dir = workspace("climb-nested");
        write(
            &dir,
            "prov.yaml",
            "title: prov config\nfields:\n  status:\n    - under: '[[Tasks]]'\n      default: open\n    - under: '[[Sub]]'\n      default: urgent\n    - under: '[[Nowhere]]'\n      default: never\n    - default: none\n  by_stem:\n    - under: '[[tasks]]'\n      default: stem\n  by_path:\n    - under: /docs/proposals/proposals.md\n      default: path\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let config = block_on(ws.effective_config(Path::new("index.md"))).unwrap();
        let opening = |parent: &str| {
            block_on(ws.defaults_for_child(Path::new("index.md"), &config, Path::new(parent)))
                .unwrap()
        };
        let s = |v: &str| Some(Value::String(v.into()));
        let deep = opening("docs/tasks/sub/index.md");
        assert_eq!(deep.get("status").cloned(), s("urgent"));
        assert_eq!(deep.get("by_stem").cloned(), s("stem"));
        assert_eq!(deep.get("by_path"), None);
        let fix = opening("docs/tasks/tasks.md");
        assert_eq!(fix.get("status").cloned(), s("open"));
        let idea = opening("docs/proposals/proposals.md");
        assert_eq!(idea.get("status").cloned(), s("none"));
        assert_eq!(idea.get("by_path").cloned(), s("path"));
        assert_eq!(opening("readme.md").get("status").cloned(), s("none"));
    }

    #[test]
    fn nested_scopes_resolve_to_the_deeper_one_and_a_dead_anchor_is_reported() {
        let dir = workspace("nested");
        write(
            &dir,
            "prov.yaml",
            "title: prov config\nfields:\n  status:\n    - under: '[[Tasks]]'\n      default: open\n    - under: '[[Sub]]'\n      default: urgent\n    - under: '[[Nowhere]]'\n      default: never\n    - default: none\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        let config = block_on(ws.effective_config(Path::new("index.md"))).unwrap();
        let scopes = block_on(ws.field_scopes_of(Path::new("index.md"), &config)).unwrap();
        let default = |doc: &str| {
            scopes
                .spec_for(&config, "status", Path::new(doc))
                .and_then(|s| s.default.clone())
        };
        assert_eq!(
            default("docs/tasks/fix.md"),
            Some(Value::String("open".into()))
        );
        assert_eq!(
            default("docs/tasks/sub/deep.md"),
            Some(Value::String("urgent".into()))
        );
        assert_eq!(default("readme.md"), Some(Value::String("none".into())));
        assert_eq!(
            default("docs/tasks/tasks.md"),
            Some(Value::String("none".into()))
        );
        assert_eq!(
            scopes.unresolved(),
            &[Unresolved {
                field: "status".into(),
                index: 2,
                under: "[[Nowhere]]".into(),
                why: "no document exists there".into(),
            }]
        );
    }
}
