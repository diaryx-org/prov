//! `create` — a new document authored as a spanning child of an existing one.
//!
//! The only verb here that brings a file into being, so it is where the
//! workspace's authoring policy is read rather than merely maintained: the
//! child inherits the parent's *shape* (combined, or a separated node beside a
//! body file), both links are authored in their own relation's reference style,
//! and an eager identity policy stamps the new document from birth.

use std::path::{Path, PathBuf};

use fig::Segment;

use crate::document::{MetaCarrier, whole_file_format};
use crate::edit::MetaEditor;
use crate::error::{Error, Result};
use crate::fs::Storage;
use crate::identity::{IdentityPolicy, Trigger};
use crate::index::IndexStore;
use crate::link;
use crate::meta::Value;
use crate::workspace::Workspace;

/// The files [`Workspace::create`] wrote. Under a combined parent this is just
/// the one document; under a **separated** parent it is the pair — the metadata
/// node the parent links, plus its sibling prose body file — so a caller (the
/// CLI) can report both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    /// The structural document: the node the parent's spanning entry points at,
    /// carrying the metadata (and, when separated, a `content` pointer). This is
    /// also the file any ID registers.
    pub node: PathBuf,
    /// The separated prose body file, present only when the new document is a
    /// separated pair (a whole-file node beside a plain body). `None` for a
    /// combined document, where the node *is* the whole file.
    pub body: Option<PathBuf>,
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Create a new document at `path` (workspace-relative) as a spanning child
    /// of `parent`: the new file declares the inverse link back to `parent`, in
    /// `parent`'s embed archetype, and `parent`'s spanning field gains the child.
    /// If the identity policy registers on create, the new document is also
    /// assigned a stable ID.
    ///
    /// The child inherits the parent's *shape*. Under a combined parent it is a
    /// single combined file at `path`. Under a **separated** parent (a whole-file
    /// metadata node with a `content` pointer) it is a separated pair: `path`
    /// becomes the prose body and a sibling `path.<meta-ext>` the metadata node
    /// — the node is the structural document the parent links to and any ID
    /// registers. A `path` with a whole-file extension is always a bare metadata
    /// document, whatever the parent.
    ///
    /// Returns the [`Created`] files: always the structural node, plus the prose
    /// body file when the child is a separated pair.
    pub async fn create(&mut self, path: &Path, parent: &Path) -> Result<Created> {
        self.create_titled(path, parent, None).await
    }

    /// [`create`](Self::create) with an explicit `title` recorded in the new
    /// document's metadata, rather than one derived from its file stem. This is
    /// the title-primary authoring entry point (`prov new "My Great Note"`):
    /// the caller slugs the title into a readable filename ([`link::slug`]) and
    /// keeps the original title — casing, spaces, and punctuation — in the
    /// document, where structure and identity live (DESIGN §1). The parent's
    /// spanning-entry label follows the title too.
    pub async fn create_with_title(
        &mut self,
        path: &Path,
        parent: &Path,
        title: &str,
    ) -> Result<Created> {
        self.create_titled(path, parent, Some(title)).await
    }

    /// [`create`](Self::create) with an explicit title for the new document,
    /// used where the file stem is a poor title — a synthesized folder-note
    /// (`index.md`) that should read as its folder (`intake.rs`). `None` falls
    /// back to the stem, the plain-`create` behavior. Authoring the title here
    /// (rather than retitling after) keeps the parent's spanning-entry *label* in
    /// step with it, since that label is taken from the child's title.
    pub(crate) async fn create_titled(
        &mut self,
        path: &Path,
        parent: &Path,
        title_override: Option<&str>,
    ) -> Result<Created> {
        let path = link::normalize(path);
        let parent = link::normalize(parent);
        let (spanning, inverse) = self.spanning_pair()?;

        let (parent_text, parent_doc) = self.load(&parent).await?;

        // The child's shape follows the parent's. `node` is always the
        // *structural* document — the file registered, linked by the parent's
        // spanning entry, and carrying the inverse link; `body`, when present, is
        // a separated prose file written beside it. Three cases:
        //  - an explicit whole-file extension on `path` → a bare metadata
        //    document (config/registry-style node, no body);
        //  - a *separated* parent (a whole-file node pointing at prose via
        //    `content`) → a separated child: `path` is the body file and its
        //    sibling `path.<meta-ext>` the metadata node that points back at it;
        //  - otherwise → a combined document inheriting the parent's fenced block
        //    (or the workspace default when the parent is a bare config file).
        let (node, node_carrier, body): (PathBuf, MetaCarrier, Option<PathBuf>) =
            match whole_file_format(&path) {
                Some(format) => (path.clone(), MetaCarrier::WholeFile(format), None),
                None => match parent_doc.carrier {
                    Some(MetaCarrier::WholeFile(format)) if parent_doc.content_attr().is_some() => {
                        let node =
                            path.with_extension(crate::document::whole_file_extension(format));
                        (node, MetaCarrier::WholeFile(format), Some(path.clone()))
                    }
                    Some(MetaCarrier::Fenced(kind)) => {
                        (path.clone(), MetaCarrier::Fenced(kind), None)
                    }
                    _ => (
                        path.clone(),
                        crate::document::frontmatter_carrier(self.default_embed_format()),
                        None,
                    ),
                },
            };

        // Refuse if either file (the node, or a separated body) already exists.
        for existing in std::iter::once(&node).chain(body.iter()) {
            if self.exists(existing).await? {
                return Err(Error::AlreadyExists(existing.to_path_buf()));
            }
        }

        // Titles for the authored links: the child's (an explicit override, else
        // from its stem) and the parent's (its own title, else derived from the
        // path).
        let title = title_override.map(str::to_owned).unwrap_or_else(|| {
            node.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let parent_title = parent_doc
            .meta
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| link::path_to_title(&parent));

        // Everything below can touch the index — authoring an id-form link
        // registers its target — so the change set (and with it the index
        // checkpoint that unwinds those registrations) opens here, before the
        // first of them, not down at the writes.
        let mut cs = self.change();

        // The child's inverse link back to the parent, authored in the `inverse`
        // relation's style (going "up"). The parent exists, so an id link
        // registers it by path.
        let up = self
            .authored_target(&inverse, &node, &parent, &parent_title, true)
            .await?;
        // The parent's spanning entry for the child, authored in the `spanning`
        // relation's style (going "down"). The node is not on disk yet, so
        // `target_exists = false` mints its id directly rather than register-by-path.
        let down = self
            .authored_target(&spanning, &parent, &node, &title, false)
            .await?;

        // Identity hook — eager policies assign an ID from birth (idempotent: an
        // id-linked child was already registered by the spanning entry above).
        // It runs *before* the node's text is authored, because a
        // frontmatter-stamping workspace has to write that id into the document
        // it is about to compose; the registry write it implies is staged by
        // `commit` either way, so the registry still lands with the documents.
        if self.identity().registration().fires_on(Trigger::Create)
            && self.index().id_for_path(&node).is_none()
        {
            let id = self.mint_unique(&node);
            self.index_mut().register(&id, &node);
        }
        // The id the document carries as its own, under a stamping mode
        // (DESIGN §5) — `None` when ids live only in the registry, or when a lazy
        // policy has not minted one for this node at all.
        let stamp = self
            .id_storage()
            .stamps_frontmatter()
            .then(|| self.index().id_for_path(&node))
            .flatten();

        // Author the node's metadata: title, its own id (when stamped), inverse
        // link, and — for a separated child — a `content` pointer at its body
        // file. A separated node is serialized from a mapping (a whole-file
        // document, valid in any format including empty JSON); a combined child
        // grows its block via the editor.
        let new_text = match (&node_carrier, &body) {
            (MetaCarrier::WholeFile(format), Some(body_path)) => {
                let body_ref = body_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                let mut map = crate::meta::Mapping::new();
                map.insert("title".into(), Value::String(title));
                if let Some(id) = &stamp {
                    map.insert("id".into(), Value::String(id.0.clone()));
                }
                map.insert(inverse.clone(), Value::String(up));
                map.insert("content".into(), Value::String(body_ref));
                crate::meta::serialize_mapping(&map, *format)?
            }
            _ => {
                let mut new_doc = MetaEditor::open_or_init("", Some(node_carrier))?;
                new_doc.set_value(&[Segment::Key("title")], fig::Value::Str(title))?;
                if let Some(id) = &stamp {
                    new_doc.set_value(&[Segment::Key("id")], fig::Value::Str(id.0.clone()))?;
                }
                new_doc.set_value(&[Segment::Key(&inverse)], fig::Value::Str(up))?;
                new_doc.render()?
            }
        };

        // The parent: append the child to its spanning field (creating it if
        // absent — `append` needs an existing sequence).
        let mut parent_editor = MetaEditor::open_or_init(&parent_text, parent_doc.carrier)?;
        let span_path = [Segment::Key(&spanning)];
        if parent_editor
            .append_value(&span_path, fig::Value::Str(down.clone()))
            .is_err()
        {
            parent_editor.set_value(&span_path, fig::Value::Seq(vec![fig::Value::Str(down)]))?;
        }
        let parent_out = parent_editor.render()?;

        // All edits computed; stage them.
        cs.write(&node, new_text);
        // A separated child's prose file starts empty (like a combined child's
        // body, which is just the synthesized block with nothing after it).
        if let Some(body_path) = &body {
            cs.write(body_path, Vec::new());
        }
        cs.write(&parent, parent_out);

        self.commit(cs).await?;
        Ok(Created { node, body })
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::super::support::*;
    use super::*;
    use crate::index::IdIndex;
    use crate::link::LinkStyle;

    // Exercises inheritance of a `fig`-dialect parent block, so it needs that
    // backend on top of the module-wide `yaml` gate.
    #[cfg(feature = "fig-lang")]
    #[test]
    fn create_links_both_directions_in_the_parents_format() {
        let dir = tempdir("create");
        write(&dir, "index.md", "```fig\ntitle = Root\n```\nbody\n");
        // Plain-relative keeps the authored links bare and deterministic.
        let w = || {
            Workspace::builder(StdFs)
                .root(&dir)
                .link_style(LinkStyle::PlainRelative)
                .build()
        };
        block_on(w().create(Path::new("notes/new.md"), Path::new("index.md"))).unwrap();

        let child = read(&dir, "notes/new.md");
        assert!(
            child.starts_with("```fig\n"),
            "child inherits the parent's archetype: {child}"
        );
        assert!(child.contains("part_of = ../index.md"), "{child}");
        let parent = read(&dir, "index.md");
        // fig ≥ 2.2 renders spliced containers as flow — the round-trippable
        // inline spelling.
        assert!(parent.contains("contents = [notes/new.md]"), "{parent}");
        assert!(parent.ends_with("body\n"), "body untouched: {parent}");
        // The result validates cleanly.
        assert_eq!(block_on(w().check("index.md")).unwrap(), vec![]);
    }

    #[cfg(feature = "fig-lang")]
    #[test]
    fn create_uses_the_workspace_default_embed_format() {
        // The parent is a config file (whole-file metadata), so the child
        // inherits no fenced archetype and falls to the workspace default —
        // here fig, not the built-in YAML.
        let dir = tempdir("embed-default");
        write(&dir, "index.yaml", "title: Root\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .default_embed_format(fig::Format::Fig)
            .build();
        block_on(w.create(Path::new("a.md"), Path::new("index.yaml"))).unwrap();
        assert!(
            read(&dir, "a.md").starts_with("```fig"),
            "{}",
            read(&dir, "a.md")
        );
    }

    #[test]
    fn create_authors_id_links_when_configured() {
        // Obsidian-style: both structural links are authored by id, and both
        // ends are registered so the links survive any later move untouched.
        let dir = tempdir("create-id");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .identity(Minter::lazy(7))
            .index(FileIndex::new(fig::Format::Yaml))
            .id_links(true)
            .build();
        block_on(w.create(Path::new("a.md"), Path::new("index.md"))).unwrap();

        let parent_id = w
            .index()
            .id_for_path(Path::new("index.md"))
            .expect("parent registered");
        let child_id = w
            .index()
            .id_for_path(Path::new("a.md"))
            .expect("child registered");
        assert!(read(&dir, "a.md").contains(&format!("part_of: id:{parent_id}")));
        assert!(read(&dir, "index.md").contains(&format!("id:{child_id}")));
        // And it still validates — id targets resolve through the registry.
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn create_stamps_the_new_documents_id_under_frontmatter_storage() {
        use crate::config::IdStorage;
        use crate::identity::Registration;

        // Stamping storage (`id_storage: both`) makes every authored document
        // self-describing: the id prov mints lands in the document as well as the
        // registry, so identity travels with the file.
        let dir = tempdir("create-stamp");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .identity(Minter::with(Registration::EAGER, 7))
            .index(FileIndex::new(fig::Format::Yaml))
            .id_storage(IdStorage::Frontmatter)
            .build();
        block_on(w.create(Path::new("a.md"), Path::new("index.md"))).unwrap();

        let child_id = w
            .index()
            .id_for_path(Path::new("a.md"))
            .expect("child registered");
        assert!(
            read(&dir, "a.md").contains(&format!("id: {child_id}")),
            "the child carries its own id: {}",
            read(&dir, "a.md")
        );
        // The stamp agrees with the registry, so nothing is left to reconcile.
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn create_leaves_documents_id_free_under_registry_storage() {
        use crate::identity::Registration;

        // The converse, and the behavior every existing vault keeps: ids live in
        // the registry alone and documents stay free of `id` clutter.
        let dir = tempdir("create-no-stamp");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .identity(Minter::with(Registration::EAGER, 7))
            .index(FileIndex::new(fig::Format::Yaml))
            .build();
        block_on(w.create(Path::new("a.md"), Path::new("index.md"))).unwrap();

        assert!(
            w.index().id_for_path(Path::new("a.md")).is_some(),
            "eager identity still registers the child"
        );
        assert!(
            !read(&dir, "a.md").contains("id:"),
            "no stamp under registry storage: {}",
            read(&dir, "a.md")
        );
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn create_stamps_the_parent_it_registers_to_author_a_link() {
        use crate::config::IdStorage;

        // The child is not the only document that earns an id here: authoring an
        // id-form link *to* the parent registers the parent too. That id has the
        // same claim to travel with its file, so the stamp has to reach documents
        // the op only touched in passing — otherwise the very first `create` in a
        // fresh vault leaves the root unstamped and `check` immediately complains.
        let dir = tempdir("create-stamp-parent");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .identity(Minter::lazy(7))
            .index(FileIndex::new(fig::Format::Yaml))
            .id_links(true)
            .id_storage(IdStorage::Frontmatter)
            .build();
        block_on(w.create(Path::new("a.md"), Path::new("index.md"))).unwrap();

        let parent_id = w
            .index()
            .id_for_path(Path::new("index.md"))
            .expect("parent registered");
        let index = read(&dir, "index.md");
        assert!(
            index.contains(&format!("id: {parent_id}")),
            "the parent carries the id it earned: {index}"
        );
        // The stamp rode the same change set as the parent's own edit (its new
        // spanning entry), so neither clobbered the other.
        assert!(index.contains("contents:"), "{index}");
        assert_eq!(block_on(w.check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn create_stamps_a_separated_childs_node_document() {
        use crate::config::IdStorage;
        use crate::identity::Registration;

        // A separated child's id belongs on its *structural node*, the file the
        // registry knows and the parent links — not on the prose body beside it.
        let dir = tempdir("create-stamp-separate");
        write(&dir, "index.yaml", "title: Root\ncontent: index.md\n");
        write(&dir, "index.md", "# Root\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .identity(Minter::with(Registration::EAGER, 7))
            .index(FileIndex::new(fig::Format::Yaml))
            .id_storage(IdStorage::Frontmatter)
            .build();
        block_on(w.create(Path::new("notes.md"), Path::new("index.yaml"))).unwrap();

        let node_id = w
            .index()
            .id_for_path(Path::new("notes.yaml"))
            .expect("node registered");
        assert!(
            read(&dir, "notes.yaml").contains(&format!("id: {node_id}")),
            "{}",
            read(&dir, "notes.yaml")
        );
        assert_eq!(read(&dir, "notes.md"), "", "the body file stays empty");
        assert_eq!(block_on(w.check("index.yaml")).unwrap(), vec![]);
    }

    #[test]
    fn create_authors_up_and_down_in_different_relation_styles() {
        use crate::link::{Addressing, ReferenceStyle, Wrapper};
        use crate::relation::{Relation, RelationSet};

        // Down (`contents`) reads like a TOC — an alias wikilink. Up (`part_of`)
        // is durable bookkeeping — a bare markdown id link. Two relations, two
        // styles, one create.
        let alias = ReferenceStyle {
            wrapper: Wrapper::Wikilink,
            addressing: Addressing::Alias,
            label: false,
            path_style: LinkStyle::default(),
        };
        let by_id = ReferenceStyle {
            wrapper: Wrapper::Markdown,
            addressing: Addressing::Id,
            label: false,
            path_style: LinkStyle::default(),
        };
        let relations = RelationSet::new()
            .with(Relation::many("contents").inverse("part_of").style(alias))
            .with(Relation::one("part_of").inverse("contents").style(by_id))
            .spanning("contents");

        let dir = tempdir("create-updown");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .relations(relations)
            .identity(Minter::lazy(7))
            .index(FileIndex::new(fig::Format::Yaml))
            .build();
        block_on(w.create(Path::new("a.md"), Path::new("index.md"))).unwrap();

        // Up: `part_of` on the child is a durable id link, and it registered the
        // parent (the id direction is what triggers registration).
        let parent_id = w
            .index()
            .id_for_path(Path::new("index.md"))
            .expect("parent registered");
        assert!(
            read(&dir, "a.md").contains(&format!("part_of: id:{parent_id}")),
            "{}",
            read(&dir, "a.md")
        );

        // Down: `contents` on the parent is a nominal alias wikilink (the child's
        // title), and — because `alias` never links-by-id — the child is *not*
        // registered. That asymmetry is by design.
        assert!(
            read(&dir, "index.md").contains("[[a]]"),
            "{}",
            read(&dir, "index.md")
        );
        assert!(
            w.index().id_for_path(Path::new("a.md")).is_none(),
            "alias down-link must not register the child"
        );
    }

    #[test]
    fn create_makes_a_separated_child_under_a_separated_parent() {
        // A separated parent is a whole-file metadata node with a `content`
        // pointer at its prose body. A new child inherits that shape: a body
        // file plus a sibling metadata node — the node is what the parent links.
        let dir = tempdir("create-separate");
        write(&dir, "index.yaml", "title: Root\ncontent: index.md\n");
        write(&dir, "index.md", "# Root\n");

        block_on(ws(&dir).create(Path::new("notes.md"), Path::new("index.yaml"))).unwrap();

        // The structural node is `notes.yaml`: title, inverse, and a `content`
        // pointer at its (empty) prose body.
        let node = read(&dir, "notes.yaml");
        assert!(node.contains("title: notes"), "{node}");
        assert!(
            node.contains("index.yaml"),
            "inverse link to parent node: {node}"
        );
        assert!(node.contains("content: notes.md"), "{node}");
        assert_eq!(read(&dir, "notes.md"), "", "the body file starts empty");
        // The parent's spanning entry points at the node, never the body file.
        let index = read(&dir, "index.yaml");
        assert!(index.contains("notes.yaml"), "{index}");
        assert!(
            !index.contains("notes.md"),
            "parent links the node, not the body: {index}"
        );
        // The whole (separated) workspace still validates.
        assert_eq!(block_on(ws(&dir).check("index.yaml")).unwrap(), vec![]);
    }

    #[test]
    fn create_with_title_keeps_the_title_distinct_from_the_slugged_stem() {
        // Title-primary authoring: the caller slugs the title into a readable
        // filename but the document records the original title verbatim (casing
        // and spaces the stem cannot carry), and the parent's entry label follows.
        let dir = tempdir("create-title");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        let stem = crate::link::slug("My Great Note");
        assert_eq!(stem, "my-great-note");
        let path = PathBuf::from(format!("{stem}.md"));

        block_on(ws(&dir).create_with_title(&path, Path::new("index.md"), "My Great Note"))
            .unwrap();

        let child = read(&dir, "my-great-note.md");
        assert!(
            child.contains("title: My Great Note"),
            "original title kept: {child}"
        );
        // The parent's spanning-entry label reads as the title, not the stem.
        assert!(
            read(&dir, "index.md").contains("My Great Note"),
            "{}",
            read(&dir, "index.md")
        );
        assert_eq!(block_on(ws(&dir).check("index.md")).unwrap(), vec![]);
    }

    #[test]
    fn create_refuses_an_existing_path() {
        let dir = tempdir("exists");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        write(&dir, "a.md", "already here\n");
        let err = block_on(ws(&dir).create(Path::new("a.md"), Path::new("index.md"))).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn eager_create_assigns_an_id_from_birth() {
        let dir = tempdir("id-eager");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .identity(Minter::eager(7))
            .index(FileIndex::new(fig::Format::Yaml))
            .build();
        block_on(w.create(Path::new("a.md"), Path::new("index.md"))).unwrap();
        let id = w
            .index()
            .id_for_path(Path::new("a.md"))
            .expect("registered at birth");
        assert!(crate::identity::verify(id.as_str()));
    }

    #[test]
    fn a_failed_create_leaves_neither_the_child_nor_the_parents_entry() {
        // `create` writes the child then the parent. Fail the parent's write and
        // the child file must not survive: a document nothing contains is exactly
        // the orphan `check` cannot see (DESIGN §8).
        let dir = tempdir("atomic-create");
        write(&dir, "index.md", "---\ntitle: Root\n---\nbody\n");
        let before = snapshot(&dir);

        let mut w = failing_ws(&dir, 1);
        let err = block_on(w.create(Path::new("a.md"), Path::new("index.md"))).unwrap_err();
        assert!(err.to_string().contains("disk full"), "{err}");
        assert_eq!(
            snapshot(&dir),
            before,
            "a failed create left something behind"
        );
    }

    #[test]
    fn consecutive_ops_keep_each_others_registrations() {
        // The other half of the checkpoint's lifetime, and the one that bites in
        // the ordinary case: a *successful* op must drop its checkpoint even when
        // it staged no registry write of its own — a host-less store (frontmatter
        // storage, or before a registry is bootstrapped), an `InMemoryIndex`, an
        // op that never dirtied the index. Otherwise the checkpoint outlives the
        // op that took it, and the next one cannot tell it from a leak.
        let dir = tempdir("consecutive-ops");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        let mut w = Workspace::builder(StdFs)
            .root(&dir)
            .identity(Minter::eager(7))
            .id_links(true)
            // No host: `pending_write` stages nothing, so `commit` has no registry
            // write to report — exactly the case that leaves a checkpoint behind.
            .index(FileIndex::new(fig::Format::Yaml))
            .build();

        block_on(w.create(Path::new("a.md"), Path::new("index.md"))).unwrap();
        let a_id = w
            .index()
            .id_for_path(Path::new("a.md"))
            .expect("a.md registered");
        let root_id = w
            .index()
            .id_for_path(Path::new("index.md"))
            .expect("root registered");

        block_on(w.create(Path::new("b.md"), Path::new("index.md"))).unwrap();

        // The second create must not have unwound the first one's registrations —
        // the root's `contents` now links both by id, and both must resolve.
        assert_eq!(
            w.index().id_for_path(Path::new("a.md")),
            Some(a_id.clone()),
            "the first op's registration was erased by the second"
        );
        assert_eq!(
            w.index().id_for_path(Path::new("index.md")),
            Some(root_id),
            "the root was re-minted a second, different id"
        );
        assert!(
            w.index().id_for_path(Path::new("b.md")).is_some(),
            "b.md registered"
        );

        // The authored links must actually resolve — the user-visible failure.
        let root_text = read(&dir, "index.md");
        assert!(
            root_text.contains(a_id.as_str()),
            "the root links a.md by id: {root_text}"
        );
        assert_eq!(
            w.index().resolve(&a_id),
            Some(PathBuf::from("a.md")),
            "the id the root links must still resolve"
        );
    }

    #[test]
    fn a_registration_made_between_ops_survives_the_next_one() {
        // `change` unwinds an outstanding checkpoint, so it must be certain that a
        // checkpoint outstanding at that moment really is abandoned work. A
        // caller registering an ID *between* two ops — the public seam
        // `Workspace::register` — is not abandoned work, and erasing it would
        // dangle any link the caller authored from the id it was handed.
        let dir = tempdir("register-between-ops");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        write(&dir, "a.md", "---\ntitle: A\npart_of: index.md\n---\n");
        let mut w = hosted_registry_ws(&dir, StdFs);

        // An op that succeeds without dirtying the index — the case that used to
        // leave a checkpoint outstanding.
        block_on(w.convert_link_style(Path::new("a.md"), LinkStyle::PlainRelative, false)).unwrap();

        let id = block_on(w.register(Path::new("a.md"), Trigger::Link)).unwrap();
        block_on(w.create(Path::new("c.md"), Path::new("index.md"))).unwrap();

        assert_eq!(
            w.index().resolve(&id),
            Some(PathBuf::from("a.md")),
            "a registration made between ops must not be erased by the next one"
        );
        assert!(
            read(&dir, "registry.yaml").contains("a.md"),
            "and the next op's commit should carry it to disk"
        );
    }

    #[test]
    fn a_dangling_checkpoint_is_unwound_by_the_next_op_not_inherited() {
        // The third window, after "the write failed" and "the rollback failed":
        // an op that returns early *between* `change` and `commit`, by `?` on an
        // edit it was still computing. Its writes never happened, but its
        // registrations did, and no commit ran to unwind them — `create` mints the
        // child's ID before authoring the parent's entry, so the leak would be a
        // registry record naming a document that was never written.
        //
        // Driven through the index protocol rather than a fixture, deliberately.
        // Reaching that `?` from the public API needs a metadata edit the editor
        // rejects *after* the mint, which the ops currently recover from or make
        // unreachable — so a fixture would either not fail at all (and quietly
        // stop testing this) or encode today's exact failure points as if they
        // were the contract. What is being asserted is `change`'s contract: a
        // checkpoint left outstanding is unwound, never inherited.
        let dir = tempdir("dangling-checkpoint");
        write(&dir, "index.md", "---\ntitle: Root\n---\n");
        let mut w = id_ws(&dir);

        // Exactly what an op that bailed after minting would leave behind.
        w.index_mut().checkpoint();
        let ghost = crate::identity::Id("ghostid".into());
        w.index_mut()
            .register(&ghost, Path::new("never-written.md"));
        assert!(w.index().is_dirty(), "the abandoned op dirtied the store");

        // The next op unwinds it rather than staging it into its own registry.
        block_on(w.create(Path::new("real.md"), Path::new("index.md"))).unwrap();
        assert_eq!(
            w.index().resolve(&ghost),
            None,
            "a document that was never created must not survive into the next op"
        );
    }
}
