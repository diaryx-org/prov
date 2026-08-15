//! `rename` — a document's path changes and every link that touched it follows.
//!
//! The op with the most to maintain: every inbound reference that resolves to
//! the old path is retargeted, every relative link the moved document itself
//! declares is recomputed, a separated document's body file travels beside its
//! node, and the registry follows the id — all as one change set, so a failure
//! anywhere leaves the workspace exactly as it was found.

use std::path::{Path, PathBuf};

use fig::Segment;

use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;
use prov_graph::document::Document;
use prov_graph::error::{Error, Result};
use prov_graph::link::{self, Link, LinkStyle};
use prov_graph::meta::Value;
use prov_store::edit::MetaEditor;
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use super::maintain::{body_sibling, content_target, splice_body};

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Move/rename the document at `from` to `to`, maintaining every affected
    /// link across the workspace. Every inbound reference that resolves to
    /// `from` by a path — the parent's spanning entry, each child's inverse,
    /// overlay `links`, and body `[[…]]` wikilinks, wherever they live — is
    /// retargeted to `to`; and, when the directory changes, every relative link
    /// the moved document itself declares (frontmatter and body alike) is
    /// recomputed. Labels on `[label](path)` links and `[[target|label]]`
    /// wikilinks are preserved. `colophon:<id>` references are left untouched:
    /// where a registry is present its `id → path` update keeps them resolving
    /// (the point of an ID link), and in a path-only (Diaryx-style) workspace
    /// they never appear.
    ///
    /// Inbound references are found by a [`census`](Workspace::census) over the
    /// spanning tree, whose root is discovered by walking `part_of` up from
    /// `from` — so the caller supplies no root. References living only in
    /// documents *unreachable* from that root are not seen (a malformed tree,
    /// which `check` reports separately).
    pub async fn rename(&mut self, from: &Path, to: &Path) -> Result<()> {
        let from = link::normalize(from);
        let to = link::normalize(to);

        if !self.exists(&from).await? {
            return Err(Error::NotFound(from.to_path_buf()));
        }
        if self.exists(&to).await? {
            return Err(Error::AlreadyExists(to.to_path_buf()));
        }

        // `to` may already be registered — a live entry the on-disk check above
        // cannot see, since `id_storage`'s default `both` lets a registry entry
        // name a path with no file behind it (index.rs's module docs). Moving
        // `from`'s id onto it would take that registration away from a document
        // whose frontmatter still spells it, the exact tear `set_path`'s
        // half-eviction cannot see either. Checked up front, beside the path
        // guard, before any of the rewrite work below is even computed.
        let moving_id = self.index().id_for_path(&from);
        if let Some(id) = &moving_id
            && let Some(conflict) = self.move_conflict(id, &to)
        {
            return Err(conflict.into());
        }

        let (from_text, from_doc) = self.load(&from).await?;
        let mut cs = self.change();

        // 1. Inbound references: every document that links *to* `from` by a
        //    path, retargeted to `to` (parent's spanning entry, children's
        //    inverses, overlay `links`, body wikilinks). Id-form links resolve
        //    through the registry and are never rewritten.
        let inbound_writes = self.collect_inbound_rewrites(&from, &to).await?;

        // A separated document's prose lives in a sibling body file; move it
        // alongside (and keep the `content` pointer correct) so the pair travels
        // together.
        let body_move = self.plan_body_move(&from_doc, &from, &to).await?;

        // The body's destination needs the same refusal as the node's. A rename
        // overwrites, and an overwrite is the one thing staging cannot make good:
        // every other op records its undo before acting, but a clobbered file's
        // bytes are gone by the time anything could have copied them. So this is
        // a guard, not something rollback covers — and it is easy to walk into,
        // since the body's name is *derived* (`notes.yaml` → `notes.md`) and so
        // never passed by the caller, who therefore never sees the collision.
        if let Some(mv) = &body_move
            && self.exists(&mv.to).await?
        {
            return Err(Error::Structure(format!(
                "{}'s content file would move to {}, which already exists",
                to.display(),
                mv.to.display()
            )));
        }

        // 2. The document itself: when its directory changes, every relative
        //    link it declares must be recomputed to keep resolving — first the
        //    frontmatter links, then the body wikilinks (whose spans MetaEditor
        //    leaves verbatim, so they can be spliced afterwards).
        let mut self_text = if from.parent() != to.parent() {
            let meta_rewritten = rerelativize(
                &from_text,
                &from_doc,
                self.relations().relations(),
                &from,
                &to,
                |field| self.reference_style_for(field).path_style,
            )?;
            rerelativize_body_links(
                &meta_rewritten,
                &from_doc.body,
                &from,
                &to,
                self.link_style(),
            )
        } else {
            from_text
        };
        // For a separated node, repoint its `content` to the (moved) body file.
        if let Some(mv) = &body_move
            && let Some(carrier) = from_doc.carrier
        {
            let mut editor = MetaEditor::open(&self_text, carrier)?;
            editor.replace_value(
                &[Segment::Key("content")],
                fig::Value::Str(mv.new_ref.clone()),
            )?;
            self_text = editor.render()?;
        }

        // All edits computed; stage them.
        cs.rename(&from, &to);
        cs.write(&to, self_text);
        if let Some(mv) = &body_move {
            cs.rename(&mv.from, &mv.to);
            // A prose body is rewritten with its re-relativized text; an opaque
            // payload (`text` is `None`) is left exactly as the rename moved it.
            if let Some(text) = &mv.text {
                cs.write(&mv.to, text.clone());
            }
        }
        for (source, text) in inbound_writes {
            cs.write(source, text);
        }

        // Identity hook — the registry follows the move, so every
        // `colophon:<id>` reference to this document survives untouched. Staged
        // with the documents: a move whose links are maintained but whose
        // registry is not is the one tear IDs exist to prevent. `to` was
        // already cleared of a foreign registration above.
        if let Some(id) = moving_id {
            self.index_mut().set_path(&id, &to);
        }
        self.commit(cs).await
    }

    /// If `from` is a separated node, plan the move of its body file to sit
    /// beside `to`, with its prose wikilinks re-relativized when the directory
    /// changes. `None` for a combined document.
    ///
    /// The body's new name follows the pair's naming convention. A separated
    /// **prose** node shares its body's stem (`notes.yaml` ↔ `notes.md`), so the
    /// body keeps its own extension on the new stem. An **attachment** node
    /// carries the whole payload name plus a metadata extension (`hero.jpg.yaml`
    /// ↔ `hero.jpg`), so the payload name *is* the node's stem — reconstructing it
    /// with the body's extension would double it (`hero.jpg.jpg`).
    async fn plan_body_move(
        &self,
        doc: &Document,
        from: &Path,
        to: &Path,
    ) -> Result<Option<BodyMove>> {
        let Some(body_from) = content_target(doc, from) else {
            return Ok(None);
        };
        let opaque = prov_graph::document::is_opaque_payload(&body_from);
        let (body_to, new_ref) = body_sibling(to, &body_from);
        // An *attachment* payload is opaque bytes (an image, a PDF) — never read
        // it as text, and never rewrite it. The bare `rename` carries the bytes;
        // `text` stays `None`. A prose body is loaded and its wikilinks
        // re-relativized when the directory changes, as before.
        let text = if opaque {
            None
        } else {
            let (raw, _) = self.load(&body_from).await?;
            Some(if from.parent() != to.parent() {
                rerelativize_body_links(&raw, &raw, &body_from, &body_to, self.link_style())
            } else {
                raw
            })
        };
        Ok(Some(BodyMove {
            from: body_from,
            to: body_to,
            new_ref,
            text,
        }))
    }
}

/// A planned move of a separated document's body file, computed during `rename`
/// (see [`Workspace::plan_body_move`]) and applied in its write phase.
struct BodyMove {
    /// The body file's current workspace-relative path.
    from: PathBuf,
    /// Where the body file moves to (beside the renamed metadata file).
    to: PathBuf,
    /// The metadata file's new `content` value — the body file's basename.
    new_ref: String,
    /// The prose body's text, wikilinks re-relativized if the directory changed,
    /// to rewrite after the move. `None` for an opaque attachment payload, whose
    /// bytes the bare rename carries untouched.
    text: Option<String>,
}

/// Recompute every relative link `doc` declares so it still resolves after the
/// document moves from `from` to `to`. External and `colophon:<id>` targets
/// are untouched — neither depends on where the document lives.
fn rerelativize(
    text: &str,
    doc: &Document,
    relations: &[prov_graph::relation::Relation],
    from: &Path,
    to: &Path,
    style_for: impl Fn(&str) -> LinkStyle,
) -> Result<String> {
    let Some(carrier) = doc.carrier else {
        return Ok(text.to_string()); // no metadata: nothing to re-relativize
    };
    let mut editor = MetaEditor::open(text, carrier)?;
    for relation in relations {
        let Some(value) = doc.meta.get(&relation.name) else {
            continue;
        };
        let style = style_for(&relation.name);
        let rewrite = |raw: &str| -> Option<String> {
            let target = Link::parse(raw);
            if !target.is_path_target() {
                return None;
            }
            let resolved = link::resolve(from, &target.target);
            let new_target = link::path_text(style, to, &resolved);
            let rendered = target.with_path(new_target).render();
            (rendered != raw).then_some(rendered)
        };
        match value {
            Value::String(raw) => {
                if let Some(updated) = rewrite(raw) {
                    editor
                        .replace_value(&[Segment::Key(&relation.name)], fig::Value::Str(updated))?;
                }
            }
            Value::Sequence(items) => {
                for (i, item) in items.iter().enumerate() {
                    if let Some(raw) = item.as_str()
                        && let Some(updated) = rewrite(raw)
                    {
                        editor.replace_value(
                            &[Segment::Key(&relation.name), Segment::Index(i)],
                            fig::Value::Str(updated),
                        )?;
                    }
                }
            }
            _ => {}
        }
    }
    editor.render()
}

/// Re-relativize the path-form body links in a moved document's body —
/// `[[wikilinks]]` and markdown/djot `[t](a)` links alike — so they still
/// resolve from `to`'s directory, then splice the rewritten body back into
/// `text` (the already-frontmatter-rewritten document). `body` is the moved
/// document's verbatim prose, which MetaEditor preserved byte-for-byte, so it is
/// still a contiguous run of `text`. Id-form (`id:<id>`) and external
/// (`scheme://…`) targets are left alone — neither depends on where the document
/// lives. Each link keeps its own wrapper on rewrite ([`Link::render`]), so a
/// wikilink stays `[[…]]` and a markdown link stays `[label](…)`. Returns `text`
/// unchanged when the body has no rewritable link.
fn rerelativize_body_links(
    text: &str,
    body: &str,
    from: &Path,
    to: &Path,
    style: LinkStyle,
) -> String {
    if body.is_empty() {
        return text.to_string();
    }
    let mut new_body = String::with_capacity(body.len());
    let mut cursor = 0;
    let mut rewrote = false;
    for bl in link::scan_body_links(from, body) {
        // ID-form (stable by construction) and external targets stay put; the
        // text between `cursor` and this span — including any such skipped
        // link — is copied verbatim by the next span's push (or the tail).
        if !bl.is_path_target() {
            continue;
        }
        let resolved = link::resolve(from, &bl.link.target);
        let new_target = link::path_text(style, to, &resolved);
        let retargeted = bl.link.with_path(new_target).render();
        if retargeted == body[bl.span.start..bl.span.end] {
            continue; // already resolves under the workspace's path style
        }
        new_body.push_str(&body[cursor..bl.span.start]);
        new_body.push_str(&retargeted);
        cursor = bl.span.end;
        rewrote = true;
    }
    if !rewrote {
        return text.to_string();
    }
    new_body.push_str(&body[cursor..]);
    splice_body(text, body, &new_body)
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use crate::identity::Trigger;
    use prov_graph::index::IdIndex;

    #[test]
    fn rename_maintains_parent_children_and_own_links() {
        let dir = tempdir("rename");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[Mid](mid.md)'\n---\n",
        );
        write(
            &dir,
            "mid.md",
            "---\n# a comment to preserve\npart_of: index.md\ncontents:\n- leaf.md\n---\nmid body\n",
        );
        write(&dir, "leaf.md", "---\npart_of: mid.md\n---\n");

        block_on(ws(&dir).rename(Path::new("mid.md"), Path::new("sub/mid.md"))).unwrap();

        // Parent entry retargeted, label kept, root-absolute (the workspace
        // default path style).
        let index = read(&dir, "index.md");
        assert!(index.contains("- '[Mid](/sub/mid.md)'"), "{index}");
        // Child's inverse retargeted.
        let leaf = read(&dir, "leaf.md");
        assert!(leaf.contains("part_of: /sub/mid.md"), "{leaf}");
        // The moved doc's own links re-relativized; comment and body kept.
        let mid = read(&dir, "sub/mid.md");
        assert!(mid.contains("part_of: /index.md"), "{mid}");
        assert!(mid.contains("- /leaf.md"), "{mid}");
        assert!(mid.contains("# a comment to preserve"), "{mid}");
        assert!(mid.ends_with("mid body\n"), "{mid}");
        // The whole workspace still validates.
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn rename_respects_an_explicit_relative_path_style() {
        // The workspace default is root-absolute (every other test in this
        // module runs under it); a workspace configured for `../`-relative
        // links must still get those out of a move — the style is an axis
        // `rename` consults, not a form it assumes.
        let dir = tempdir("rename-relative-style");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- '[Mid](mid.md)'\n---\n",
        );
        write(
            &dir,
            "mid.md",
            "---\npart_of: index.md\ncontents:\n- leaf.md\n---\nSee [[leaf.md]].\n",
        );
        write(&dir, "leaf.md", "---\npart_of: mid.md\n---\n");

        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .link_style(LinkStyle::MarkdownRelative)
            .build();
        block_on(w.rename(Path::new("mid.md"), Path::new("sub/mid.md"))).unwrap();

        // Inbound references from root-level documents: no leading `/`.
        let index = read(&dir, "index.md");
        assert!(index.contains("- '[Mid](sub/mid.md)'"), "{index}");
        let leaf = read(&dir, "leaf.md");
        assert!(leaf.contains("part_of: sub/mid.md"), "{leaf}");
        // The moved doc's own links, re-relativized from its new directory.
        let mid = read(&dir, "sub/mid.md");
        assert!(mid.contains("part_of: ../index.md"), "{mid}");
        assert!(mid.contains("- ../leaf.md"), "{mid}");
        assert!(mid.contains("[[../leaf.md]]"), "{mid}");
    }

    #[test]
    fn rename_retargets_every_inbound_reference_not_just_the_first() {
        // One document may reference the same target many times — a chapter of
        // scripture cites another chapter once per verse, each with its own
        // `#verse` locator. Rewriting only the first left the rest pointing at
        // the path the move had just emptied, so the move itself authored the
        // broken links `check` then reported.
        let dir = tempdir("rename-many-inbound");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n- b.md\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\npart_of: index.md\nlinks:\n- '[B 1](b.md#1)'\n- '[B 2](b.md#2)'\n\
             - '[elsewhere](index.md)'\n- '[B 3](b.md#3)'\n---\nAlso [[b.md#4|B 4]].\n",
        );
        write(&dir, "b.md", "---\npart_of: index.md\n---\n");

        block_on(ws(&dir).rename(Path::new("b.md"), Path::new("sub/b.md"))).unwrap();

        let a = read(&dir, "a.md");
        // Every one of the three frontmatter references moved, each keeping its
        // own locator and label…
        assert!(a.contains("- '[B 1](/sub/b.md#1)'"), "{a}");
        assert!(a.contains("- '[B 2](/sub/b.md#2)'"), "{a}");
        assert!(a.contains("- '[B 3](/sub/b.md#3)'"), "{a}");
        // …the body wikilink too, and the unrelated entry was left alone.
        assert!(a.contains("[[/sub/b.md#4|B 4]]"), "{a}");
        assert!(a.contains("- '[elsewhere](index.md)'"), "{a}");
        assert!(
            !a.contains("(b.md#"),
            "a stale target survived the move: {a}"
        );
        // And the workspace the move produced is itself clean.
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn rename_rerelativizes_path_wikilinks_and_spares_id_ones() {
        // The identity-free (Diaryx-style) half: a moved document's body
        // wikilinks are maintained by rewriting the path form, while a
        // `[[colophon:id]]` reference is left exactly as written.
        let dir = tempdir("wikilink-rerel");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- mid.md\n---\n",
        );
        write(
            &dir,
            "mid.md",
            "---\npart_of: index.md\n---\nSee [[leaf.md|the leaf]] and [[colophon:ajp7eqb|pinned]].\n",
        );
        write(&dir, "leaf.md", "---\ntitle: Leaf\n---\n");

        block_on(ws(&dir).rename(Path::new("mid.md"), Path::new("sub/mid.md"))).unwrap();

        let mid = read(&dir, "sub/mid.md");
        // Path wikilink re-relativized (label kept) so it still reaches leaf.md,
        // root-absolute (the workspace default path style).
        assert!(mid.contains("[[/leaf.md|the leaf]]"), "{mid}");
        // ID wikilink untouched — location-independent by construction.
        assert!(mid.contains("[[colophon:ajp7eqb|pinned]]"), "{mid}");
        // Frontmatter maintenance still holds, and the prose survives verbatim.
        assert!(mid.contains("part_of: /index.md"), "{mid}");
        assert!(mid.ends_with(".\n"), "body preserved: {mid}");
        // Parent's spanning entry followed the move too.
        assert!(
            read(&dir, "index.md").contains("sub/mid.md"),
            "parent retargeted"
        );
    }

    #[test]
    fn rename_rerelativizes_markdown_body_links_and_spares_external_and_code() {
        // Stage 2: real markdown `[label](path)` links in body prose are now
        // maintained on a move, just like wikilinks — while an external URL and a
        // link that is actually code (inside a fence) are left untouched.
        let dir = tempdir("md-body-rerel");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- mid.md\n---\n",
        );
        write(
            &dir,
            "mid.md",
            "---\npart_of: index.md\n---\n\
             See [the leaf](leaf.md) and [home](https://ex.com).\n\n\
             ```\n[fake](leaf.md)\n```\n",
        );
        write(&dir, "leaf.md", "---\ntitle: Leaf\n---\n");

        block_on(ws(&dir).rename(Path::new("mid.md"), Path::new("sub/mid.md"))).unwrap();

        let mid = read(&dir, "sub/mid.md");
        // The inline markdown link was re-relativized, label kept, wrapper kept,
        // root-absolute (the workspace default path style).
        assert!(mid.contains("[the leaf](/leaf.md)"), "{mid}");
        // The external URL is untouched.
        assert!(mid.contains("[home](https://ex.com)"), "{mid}");
        // The look-alike link inside the code fence must NOT be rewritten.
        assert!(
            mid.contains("[fake](leaf.md)"),
            "code fence left alone: {mid}"
        );
        assert!(
            read(&dir, "index.md").contains("sub/mid.md"),
            "parent retargeted"
        );
    }

    #[test]
    fn rename_leaves_cross_workspace_references_exactly_as_written() {
        // A move re-relativizes what says where it lives. A cross-workspace
        // reference does not: the qualifier names a workspace, not a directory,
        // so re-relativizing one would turn a valid reference into a path into
        // nowhere. Both carriers — frontmatter relation and body prose — and
        // both wrappers.
        let dir = tempdir("foreign-survives-rename");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- mid.md\n---\n",
        );
        write(
            &dir,
            "mid.md",
            "---\npart_of: index.md\nlinks:\n- id:notes/ajp7eq\n- '[Their Note](id:diaryx/xk4m2p)'\n---\n\
             See [[id:notes/ajp7eq|Their Note]] and [that](id:diaryx/xk4m2p), plus [the leaf](leaf.md).\n",
        );
        write(&dir, "leaf.md", "---\ntitle: Leaf\n---\n");

        block_on(ws(&dir).rename(Path::new("mid.md"), Path::new("sub/mid.md"))).unwrap();

        let mid = read(&dir, "sub/mid.md");
        // Byte-identical, in every position it appeared.
        assert!(mid.contains("- id:notes/ajp7eq"), "frontmatter bare: {mid}");
        assert!(
            mid.contains("[Their Note](id:diaryx/xk4m2p)"),
            "frontmatter labeled: {mid}"
        );
        assert!(
            mid.contains("[[id:notes/ajp7eq|Their Note]]"),
            "body wikilink: {mid}"
        );
        assert!(
            mid.contains("[that](id:diaryx/xk4m2p)"),
            "body markdown: {mid}"
        );
        // The ordinary path link beside them still moved, so the pass ran at all
        // — without this the test would pass on a rename that did nothing.
        assert!(mid.contains("[the leaf](/leaf.md)"), "control: {mid}");
    }

    #[test]
    fn rename_retargets_inbound_markdown_body_links() {
        // A sibling references the moved doc with a markdown body link; the census
        // finds it and the move retargets it — the inbound direction, for markdown.
        let dir = tempdir("md-body-inbound");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n- b.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        write(
            &dir,
            "b.md",
            "---\npart_of: index.md\n---\nAlso see [it](a.md) nearby.\n",
        );

        block_on(ws(&dir).rename(Path::new("a.md"), Path::new("sub/a.md"))).unwrap();

        assert!(
            read(&dir, "b.md").contains("[it](/sub/a.md)"),
            "inbound md link retargeted: {}",
            read(&dir, "b.md")
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn same_directory_rename_leaves_body_wikilinks_alone() {
        // Outbound links resolve from the document's *directory*; a same-dir
        // rename does not move them, so the body must not churn.
        let dir = tempdir("wikilink-samedir");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n",
        );
        write(
            &dir,
            "a.md",
            "---\npart_of: index.md\n---\nlink to [[leaf.md]].\n",
        );
        write(&dir, "leaf.md", "---\ntitle: Leaf\n---\n");

        block_on(ws(&dir).rename(Path::new("a.md"), Path::new("b.md"))).unwrap();
        assert!(
            read(&dir, "b.md").contains("[[leaf.md]]"),
            "unchanged in-place"
        );
    }

    #[test]
    fn rename_retargets_overlay_and_body_inbound_links_anywhere() {
        // A sibling — neither parent nor child of the moved doc — references it
        // two ways: an overlay `links` relation and a body wikilink. Both must
        // follow the move; the census finds them where the old local spanning
        // walk never would. Identity-free: pure Diaryx-style path links.
        let dir = tempdir("inbound");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n- b.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        write(
            &dir,
            "b.md",
            "---\npart_of: index.md\nlinks:\n- a.md\n---\nAlso see [[a.md]] nearby.\n",
        );

        block_on(ws(&dir).rename(Path::new("a.md"), Path::new("sub/a.md"))).unwrap();

        // Parent's spanning entry followed the move (as the old code did too).
        assert!(
            read(&dir, "index.md").contains("sub/a.md"),
            "parent retargeted"
        );
        let b = read(&dir, "b.md");
        // Overlay `links` inbound from a sibling — newly maintained,
        // root-absolute (the workspace default path style).
        assert!(b.contains("- /sub/a.md"), "overlay links retargeted: {b}");
        // Body wikilink inbound from a sibling — newly maintained.
        assert!(b.contains("[[/sub/a.md]]"), "body wikilink retargeted: {b}");
        // The moved doc's own inverse re-relativized from its new location.
        assert!(read(&dir, "sub/a.md").contains("part_of: /index.md"));
        // The whole workspace still validates.
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn id_links_survive_a_rename_without_any_text_edit() {
        let dir = tempdir("id-rename");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n",
        );
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");

        let mut w = id_ws(&dir);
        // Author a link-by-id: register, then write the id target into index.md.
        let id = block_on(w.register(Path::new("a.md"), Trigger::Link)).unwrap();
        let text = read(&dir, "index.md");
        let carrier = Document::parse("index.md", &text).unwrap().carrier;
        let updated = prov_store::edit::set_in_text(
            &text,
            carrier,
            "contents.0",
            fig::Value::Str(link::id_target(&id)),
        )
        .unwrap();
        std::fs::write(dir.join("index.md"), &updated).unwrap();

        // The id target resolves in traversal and validation.
        let tree = block_on(w.tree("index.md")).unwrap();
        assert_eq!(tree.children[0].path, PathBuf::from("a.md"));
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);

        // Move the target. The parent's id entry must NOT be rewritten; the
        // registry follows instead.
        block_on(w.rename(Path::new("a.md"), Path::new("sub/a.md"))).unwrap();
        let index_text = read(&dir, "index.md");
        assert!(
            index_text.contains(&format!("id:{id}")),
            "id entry untouched: {index_text}"
        );
        assert_eq!(w.index().resolve(&id), Some(PathBuf::from("sub/a.md")));
        let tree = block_on(w.tree("index.md")).unwrap();
        assert_eq!(tree.children[0].path, PathBuf::from("sub/a.md"));
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn paths_only_workspace_is_untouched_by_the_identity_layer() {
        // The additive claim, negatively: the same mutations on a NoIdentity/
        // NoIndex workspace compile and run with the hooks monomorphized away.
        let dir = tempdir("no-id");
        write(&dir, "index.md", "---\ncontents:\n- a.md\n---\n");
        write(&dir, "a.md", "---\npart_of: index.md\n---\n");
        let mut w = ws(&dir);
        block_on(w.rename(Path::new("a.md"), Path::new("b.md"))).unwrap();
        block_on(w.delete(Path::new("b.md"), false)).unwrap();
        assert_eq!(w.index().id_for_path(Path::new("b.md")), None);
    }

    #[test]
    fn moving_a_separated_node_refuses_to_overwrite_an_occupied_body_path() {
        // `rename` guards its own destination but not its *body's*, and a rename
        // that clobbers is the one thing a change set cannot undo: the overwritten
        // bytes are gone before any rollback could copy them. So the guard has to
        // be a refusal up front, alongside the check on the node's own path.
        let dir = tempdir("body-collision");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- notes.yaml\n---\n",
        );
        write(
            &dir,
            "notes.yaml",
            "title: Notes\npart_of: index.md\ncontent: notes.md\n",
        );
        write(&dir, "notes.md", "the prose\n");
        // An unrelated document already sitting where the body would land.
        write(&dir, "other.md", "PRECIOUS — must not be destroyed\n");

        let err = block_on(ws(&dir).rename(Path::new("notes.yaml"), Path::new("other.yaml")))
            .unwrap_err();
        assert!(
            err.to_string().contains("other.md"),
            "should name the blocker: {err}"
        );
        assert_eq!(
            read(&dir, "other.md"),
            "PRECIOUS — must not be destroyed\n",
            "the move destroyed an unrelated document"
        );
        assert!(
            dir.join("notes.yaml").exists(),
            "and the refused move changed nothing"
        );
    }

    #[test]
    fn a_failed_rename_leaves_every_inbound_link_pointing_at_the_original() {
        // The op with the most writes and the most to lose: the file moves, then
        // the parent's entry, the sibling's overlay link and the body wikilink all
        // retarget. Fail each write in turn — the workspace must come back whole
        // every time, whichever one it was.
        //
        // The sweep is bounded by a probe run rather than a literal, so it keeps
        // covering every write the day `rename` grows one.
        let probe = tempdir("atomic-rename-probe");
        let _ = std::fs::remove_dir_all(&probe);
        let dir = linked_tree("atomic-rename-probe");
        let mut w = Workspace::builder(FailAtWrite::never()).root(&dir).build();
        block_on(w.rename(Path::new("a.md"), Path::new("sub/a.md"))).unwrap();
        let writes = w.fs().attempted();
        assert!(
            writes >= 3,
            "expected the move, the parent and the sibling: {writes}"
        );

        for fail_at in 0..writes {
            let dir = linked_tree("atomic-rename");
            let before = snapshot(&dir);

            let mut w = failing_ws(&dir, fail_at);
            let err = block_on(w.rename(Path::new("a.md"), Path::new("sub/a.md"))).unwrap_err();
            assert!(err.to_string().contains("disk full"), "{err}");
            assert_eq!(
                snapshot(&dir),
                before,
                "a rename that failed at write {fail_at} of {writes} left the workspace torn"
            );
        }
    }

    // ---- the registry lands with the documents (DESIGN §5) ----

    #[test]
    fn moving_the_registry_document_does_not_resurrect_it_at_its_old_path() {
        // The registry document is a document — reached from the root, movable
        // like any other. But `commit` stages the registry's own write *last*, so
        // unless that write follows the move it lands at the old path — recreating
        // the file the op just renamed away from, with all the records in it, while
        // the file the root now points at has none.
        let dir = tempdir("move-registry");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nregistry: registry.yaml\n---\n",
        );
        let mut w = hosted_registry_ws(&dir, StdFs);
        // Give the registry document an id of its own, so the move dirties the
        // store and forces the registry write into the same set as the rename.
        let id = block_on(w.register(Path::new("registry.yaml"), Trigger::Link)).unwrap();

        block_on(w.rename(Path::new("registry.yaml"), Path::new("meta/registry.yaml"))).unwrap();

        assert!(
            !dir.join("registry.yaml").exists(),
            "the registry was resurrected at the path it just moved away from"
        );
        let moved = read(&dir, "meta/registry.yaml");
        assert!(
            moved.contains(id.as_str()) && moved.contains("meta/registry.yaml"),
            "the moved registry must hold its records, repointed: {moved}"
        );
    }

    #[test]
    fn an_op_that_rewrites_the_registry_document_is_not_clobbered_by_its_own_records() {
        // The same hazard when a store *does* carry a link (machinery gets no
        // `part_of` by default, but a hand-added one — as here — must still be
        // maintained): moving the *root* re-relativizes the registry's `part_of`,
        // staging a write to the registry document. `commit` then stages its own
        // write to that document, rendered from the text it read at startup, and
        // last-write-wins would silently drop the re-relativized link.
        let dir = tempdir("rewrite-registry");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nregistry: registry.yaml\ncontents:\n- registry.yaml\n---\n",
        );
        let mut w = hosted_registry_ws(&dir, StdFs);
        // The registry document points back at the root, in a path form a move
        // must recompute.
        write(
            &dir,
            "registry.yaml",
            "title: ID registry\npart_of: index.md\n",
        );
        let text = read(&dir, "registry.yaml");
        w.index_mut().set_host("registry.yaml", &text).unwrap();
        let root_id = block_on(w.register(Path::new("index.md"), Trigger::Link)).unwrap();

        block_on(w.rename(Path::new("index.md"), Path::new("docs/index.md"))).unwrap();

        let registry = read(&dir, "registry.yaml");
        assert!(
            registry.contains("part_of: /docs/index.md"),
            "the registry document's own part_of must survive the root's move: {registry}"
        );
        assert!(
            registry.contains(root_id.as_str()),
            "and its records must be there too: {registry}"
        );
    }

    #[test]
    fn a_rename_lands_its_registry_update_in_the_same_change_set() {
        // The positive half: after a successful move the registry on disk already
        // names the new path. Nothing else had to write it — no post-hoc save
        // step, which is the window this closes.
        let dir = tempdir("registry-with-docs");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n",
        );
        write(&dir, "a.md", "---\ntitle: A\npart_of: index.md\n---\n");
        let mut w = hosted_registry_ws(&dir, StdFs);

        let id = block_on(w.register(Path::new("a.md"), Trigger::Link)).unwrap();
        block_on(w.rename(Path::new("a.md"), Path::new("moved.md"))).unwrap();

        let registry = read(&dir, "registry.yaml");
        assert!(
            registry.contains("moved.md") && !registry.contains(" a.md"),
            "the registry on disk should already name the new path: {registry}"
        );
        assert!(
            registry.contains(id.as_str()),
            "the id should be recorded: {registry}"
        );
        assert!(
            !w.index().is_dirty(),
            "a staged registry write leaves the store clean"
        );
    }

    #[test]
    fn a_failed_rename_does_not_leave_the_registry_ahead_of_the_documents() {
        // The tear this exists to prevent, and the one the documents cannot
        // self-heal from: the registry is authoritative, not derived, so an
        // `id → path` that moved while the documents did not would resolve every
        // `colophon:<id>` reference to a file that is not there.
        //
        // Swept across every write the op makes rather than aimed at one, because
        // the interesting failure is the *last* — the registry's own write, with
        // every document already on disk behind it. Fixing a single index here
        // would silently stop testing that the day an op grows a write.
        let seed = |tag: &str| {
            let dir = tempdir(tag);
            write(
                &dir,
                "index.md",
                "---\ntitle: Root\ncontents:\n- a.md\n---\n",
            );
            write(&dir, "a.md", "---\ntitle: A\npart_of: index.md\n---\n");
            dir
        };

        // Probe: how many writes does the move make, registry included?
        let dir = seed("registry-rollback-probe");
        let mut w = hosted_registry_ws(&dir, FailAtWrite::never());
        block_on(w.register(Path::new("a.md"), Trigger::Link)).unwrap();
        let before_move = w.fs().attempted();
        block_on(w.rename(Path::new("a.md"), Path::new("moved.md"))).unwrap();
        let writes = w.fs().attempted() - before_move;
        assert!(
            read(&dir, "registry.yaml").contains("moved.md"),
            "the probe should have staged the registry write — otherwise this \
             test's premise is gone"
        );

        for fail_at in 0..writes {
            let dir = seed("registry-rollback");

            // Register and let it land, so the sweep isolates the *move*.
            let mut w = hosted_registry_ws(&dir, StdFs);
            let id = block_on(w.register(Path::new("a.md"), Trigger::Link)).unwrap();
            block_on(w.create(Path::new("settle.md"), Path::new("index.md"))).unwrap();
            assert!(
                read(&dir, "registry.yaml").contains("a.md"),
                "registry seeded"
            );
            let before = snapshot(&dir);

            // Rebuild over a backend that fails this run's `fail_at`th write,
            // carrying the same on-disk registry — as a second CLI run would.
            let mut w = hosted_registry_ws(&dir, FailAtWrite::nth(fail_at));
            let id_again = block_on(w.register(Path::new("a.md"), Trigger::Link)).unwrap();
            assert_eq!(id, id_again, "the same document keeps its id across runs");

            let err = block_on(w.rename(Path::new("a.md"), Path::new("moved.md"))).unwrap_err();
            assert!(err.to_string().contains("disk full"), "{err}");

            // On disk: nothing moved — least of all the registry.
            assert_eq!(
                snapshot(&dir),
                before,
                "a rename that failed at write {fail_at} of {writes} left the workspace torn"
            );
            // In memory: the store was rolled back too, so a caller holding this
            // workspace does not go on believing the move happened.
            assert_eq!(
                w.index().resolve(&id),
                Some(PathBuf::from("a.md")),
                "the in-memory registry should have rolled back with the writes \
                 (failed at write {fail_at} of {writes})"
            );
        }
    }

    #[test]
    fn rename_refuses_to_take_a_path_the_registry_binds_to_a_different_id() {
        // The half-synced state the module docs describe (index.rs): the
        // registry still binds `p2.md` to a different document's id, even
        // though nothing sits there on disk right now — out of band, or never
        // landed. Renaming `p1.md` onto it would move `a`'s registration over
        // `b`'s without evicting `b`'s forward entry, leaving both ids
        // resolving into the same path (every `id:b` link silently wrong).
        // Refused before anything is computed, let alone moved.
        let dir = tempdir("rename-path-collision");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\ncontents:\n- p1.md\n---\n",
        );
        write(&dir, "p1.md", "---\ntitle: P1\npart_of: index.md\n---\n");

        let mut w = id_ws(&dir);
        let a = prov_graph::identity::Id("aaaaaaa".into());
        let b = prov_graph::identity::Id("bbbbbbb".into());
        w.index_mut().register(&a, Path::new("p1.md"));
        // p2.md's file was never created here: an ordinary half-synced state,
        // not a bug — a registry entry naming a path with no file behind it.
        w.index_mut().register(&b, Path::new("p2.md"));

        let err = block_on(w.rename(Path::new("p1.md"), Path::new("p2.md"))).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Collision(prov_graph::index::Collision::Path { .. })
            ),
            "{err:?}"
        );
        assert!(
            err.to_string().contains("p2.md") && err.to_string().contains("bbbbbbb"),
            "the message must name the path and what it already carries: {err}"
        );

        // Refused up front: not a byte moved, and both registrations intact.
        assert!(dir.join("p1.md").exists());
        assert!(!dir.join("p2.md").exists());
        assert_eq!(w.index().resolve(&a), Some(PathBuf::from("p1.md")));
        assert_eq!(w.index().resolve(&b), Some(PathBuf::from("p2.md")));
    }

    #[test]
    fn rename_maintains_links_to_a_document_that_declares_no_parent() {
        // The `about` page's shape: reached by the root's `about` pointer, and
        // declaring no `part_of` of its own, so it sits in no spanning tree. The
        // inbound census walks `part_of` *up* from the renamed file, and a walk
        // that cannot move used to answer "about.md" — a one-document workspace,
        // in which the root's pointer is invisible and survives the rename naming
        // a path that no longer exists.
        let dir = tempdir("rename-parentless");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nabout: about.md\ncontents:\n- a.md\n---\nSee [the page](about.md).\n",
        );
        write(&dir, "about.md", "---\ntitle: About\n---\nprose\n");
        write(&dir, "a.md", "---\ntitle: A\npart_of: index.md\n---\n");

        block_on(ws(&dir).rename(Path::new("about.md"), Path::new("guide.md"))).unwrap();

        let root = read(&dir, "index.md");
        assert!(
            root.contains("about: /guide.md"),
            "the pointer followed the move: {root}"
        );
        assert!(
            root.contains("[the page](/guide.md)"),
            "and so did the body link: {root}"
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn renaming_the_root_still_roots_its_own_census() {
        // The other document that declares no `part_of` is the root itself, where
        // "the walk did not move" is the correct answer. Its children's `part_of`
        // entries must still be retargeted — the fallback must not send the census
        // somewhere else, or hand back a root that no longer exists.
        let dir = tempdir("rename-root");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- a.md\n---\n",
        );
        write(&dir, "a.md", "---\ntitle: A\npart_of: index.md\n---\n");

        block_on(ws(&dir).rename(Path::new("index.md"), Path::new("home.md"))).unwrap();

        assert!(
            read(&dir, "a.md").contains("part_of: /home.md"),
            "the child's up-link followed the root"
        );
        assert_eq!(block_on(ws(&dir).check("home.md")).unwrap(), vec![]);
    }
}
