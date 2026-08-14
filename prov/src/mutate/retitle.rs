//! `retitle` — a document's title changes and every inbound *label* follows.
//!
//! The label half of link maintenance, the complement of
//! [`rename`](super::rename)'s target half: a link's target is left exactly as
//! written (an `id:<id>` handle or a path), while the human label every labeled
//! inbound link carries is refreshed — which is what lets a workspace author
//! `[Title](id:…)` links whose label stays honest as titles evolve.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;
use prov_graph::document::Document;
use prov_graph::error::{Error, Result};
use prov_graph::graph::Target;
use prov_graph::link::{self, Link};
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Change the document's title, refreshing the display *label* of every
    /// labeled inbound link that points at it — by id or by path — to match.
    ///
    /// The document is neither moved nor re-identified: only its `title` field
    /// and the labels of links targeting it change. A link's target (an
    /// `id:<id>` handle or a path) is left exactly as written — this is the
    /// label half of link maintenance, the complement of [`rename`](Self::rename)'s
    /// target half, and the reason a workspace can author `[Title](id:…)` links
    /// whose human label stays honest as titles evolve. Bare, unlabeled links
    /// are left alone (no label to keep in sync), and a label already equal to
    /// `new_title` is skipped (idempotent).
    ///
    /// Returns the number of inbound links relabeled. Body-prose wikilink labels
    /// are not rewritten here yet (a follow-up; a `StaleLabel` finding can flag
    /// them in the meantime).
    pub async fn retitle(&mut self, path: &Path, new_title: &str) -> Result<usize> {
        let path = link::normalize(path);
        if !self.exists(&path).await? {
            return Err(Error::NotFound(path.to_path_buf()));
        }
        let (text, doc) = self.load(&path).await?;
        let Some(carrier) = doc.carrier else {
            return Err(Error::Structure(format!(
                "{} has no metadata block to hold a title",
                path.display()
            )));
        };

        let mut cs = self.change();

        // 1. The document's own title.
        let mut editor = MetaEditor::open(&text, carrier)?;
        editor.set_value(
            &[Segment::Key("title")],
            fig::Value::Str(new_title.to_string()),
        )?;
        cs.write(&path, editor.render()?);

        // 2. Inbound labels — every document that links here with a label,
        //    refreshed to the new title.
        let relabels = self.collect_inbound_relabels(&path, new_title).await?;
        let count = relabels.len();
        for (source, updated) in relabels {
            cs.write(&source, updated);
        }

        self.commit(cs).await?;
        Ok(count)
    }

    /// Every document that links to `path` with a *labeled* link, its inbound
    /// labels refreshed to `new_title`. Mirrors [`collect_inbound_rewrites`],
    /// but keeps id-form links (which retargeting skips): a retitle refreshes the
    /// label of both id- and path-addressed links, since the label is the same
    /// human title either way. `resolved_path` collapses both forms to the target
    /// path, so one filter catches them.
    async fn collect_inbound_relabels(
        &self,
        path: &Path,
        new_title: &str,
    ) -> Result<Vec<(PathBuf, String)>> {
        let (_spanning, inverse) = self.spanning_pair()?;
        let root = self.spanning_root(path, &inverse).await?;
        let mut sources: BTreeSet<PathBuf> = self
            .census(&root)
            .await?
            .into_iter()
            .filter(|e| e.resolution.resolved_path().map(PathBuf::as_path) == Some(path))
            .map(|e| e.source)
            .collect();
        sources.remove(path);
        let mut writes = Vec::new();
        for source in sources {
            if let Some(updated) = self.relabel_inbound_doc(&source, path, new_title).await? {
                writes.push((source, updated));
            }
        }
        Ok(writes)
    }

    /// One inbound document's labels refreshed: every relation entry that
    /// resolves to `target` and carries a label distinct from `new_title` gets
    /// that label set to `new_title`, its target and wrapper untouched. Returns
    /// the rewritten text, or `None` when nothing needed changing.
    pub(crate) async fn relabel_inbound_doc(
        &self,
        source: &Path,
        target: &Path,
        new_title: &str,
    ) -> Result<Option<String>> {
        let (original, _) = self.load(source).await?;
        let mut text = original.clone();
        for relation in self.relations().relations() {
            let doc = Document::parse(source, &text)?;
            let Some(carrier) = doc.carrier else {
                return Ok(None);
            };
            let meta = fig::Value::from(&doc.meta);
            let Some(value) = meta.get(relation.name.as_str()) else {
                continue;
            };
            let is_seq = value.as_seq().is_some();
            let items = prov_graph::meta::link_strings(value);

            // Which entries resolve to `target`, carry a label, and are stale.
            let edits: Vec<(usize, String)> = items
                .iter()
                .enumerate()
                .filter_map(|(i, raw)| {
                    let link = Link::parse(raw);
                    if link.label.is_none() || link.label.as_deref() == Some(new_title) {
                        return None;
                    }
                    if self.resolve_link(source, &link) != Target::Path(target.to_path_buf()) {
                        return None;
                    }
                    Some((i, link.with_label(new_title).render()))
                })
                .collect();
            if edits.is_empty() {
                continue;
            }

            let mut editor = MetaEditor::open(&text, carrier)?;
            for (i, rendered) in edits {
                let path = if is_seq {
                    vec![Segment::Key(relation.name.as_str()), Segment::Index(i)]
                } else {
                    vec![Segment::Key(relation.name.as_str())]
                };
                editor.replace_value(&path, fig::Value::Str(rendered))?;
            }
            text = editor.render()?;
        }
        Ok((text != original).then_some(text))
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use prov_graph::link::LinkStyle;

    #[test]
    fn retitle_refreshes_inbound_id_link_labels() {
        use prov_graph::link::{Addressing, ReferenceStyle, Wrapper};
        use prov_graph::relation::{Relation, RelationSet};

        // `part_of` is authored as a *labeled id link*: `[Parent Title](id:…)`.
        // The id is the durable target; the label is the parent's title, which
        // `retitle` must keep in step.
        let by_id_labeled = ReferenceStyle {
            wrapper: Wrapper::Markdown,
            addressing: Addressing::Id,
            label: true,
            path_style: LinkStyle::default(),
        };
        let relations = RelationSet::new()
            .with(Relation::many("contents").inverse("part_of"))
            .with(
                Relation::one("part_of")
                    .inverse("contents")
                    .style(by_id_labeled),
            )
            .spanning("contents");

        let dir = tempdir("retitle");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .relations(relations)
            .identity(Minter::eager(7))
            .index(FileIndex::new(fig::Format::Yaml))
            .build();
        block_on(w.create_with_title(Path::new("child.md"), Path::new("index.md"), "Child"))
            .unwrap();

        // The child links up to the parent by id, labeled with the parent's title.
        assert!(
            read(&dir, "child.md").contains("[Root](id:"),
            "{}",
            read(&dir, "child.md")
        );

        // Retitle the parent: its title changes and the child's inbound label
        // follows, while the id target is left untouched.
        let n = block_on(w.retitle(Path::new("index.md"), "Home Base")).unwrap();
        assert_eq!(n, 1, "one inbound label refreshed");
        assert!(
            read(&dir, "index.md").contains("Home Base"),
            "title updated: {}",
            read(&dir, "index.md")
        );
        let child = read(&dir, "child.md");
        assert!(
            child.contains("[Home Base](id:"),
            "label refreshed: {child}"
        );
        assert!(!child.contains("[Root]"), "old label gone: {child}");

        // Idempotent: retitling to the same value relabels nothing.
        assert_eq!(
            block_on(w.retitle(Path::new("index.md"), "Home Base")).unwrap(),
            0
        );
    }
}
