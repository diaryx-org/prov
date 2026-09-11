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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use prov_graph::error::Result;
use prov_graph::fs::ReadStorage;
use prov_graph::graph::{NodeKind, Target, TreeOptions};
use prov_graph::index::IdIndex;
use prov_graph::link::{self, Link};
use prov_graph::meta::Mapping;
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
    use prov_graph::meta::Value;
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
