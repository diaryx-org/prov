//! The link work a walk does for each document, done ahead of it.
//!
//! The census walk visits documents in an order its findings depend on, so it
//! runs on one thread. Most of what it did per document, though, depends on
//! nothing but that document and the workspace's settings: which of its
//! metadata fields are links, what each link says, and where it points before
//! anything is asked of the filesystem or the id index. That is [`Prepared`],
//! and [`prepare`] makes it. The prefetch makes it on rayon's pool beside the
//! parse, the read memo keeps it beside the document, and the walk is left
//! the two questions only it can ask: is the file there
//! ([`exact_name`](super::Graph), against a listing the operation shares),
//! and what does the registry say about an id.
//!
//! A [`Pre`] is deliberately less than a resolution. The walk's forward
//! resolver and the inverse check's resolver disagree on an id that does not
//! verify and on a malformed one, and a nominal (`[[name]]`) link needs the
//! title index, which is built on first sight of one; so each resolver turns a
//! `Pre` into its own answer, and a nominal link is handed back to the
//! resolver whole.

use std::path::{Path, PathBuf};

use super::ReadSettings;
use super::census::FrontmatterLink;
use crate::document::Document;
use crate::identity::Id;
use crate::link::{self, BodyLink, IdRef, Link};
use crate::title;

/// How far a link resolves on what the link and the settings say alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pre {
    /// A URL or a mail address.
    External,
    /// Only a locator: a place in the document it is written in.
    SameDocument,
    /// An id in another workspace.
    Foreign { workspace: String, id: Id },
    /// `id:` with a body that is not an id.
    Malformed,
    /// An id in this workspace, for the registry to place.
    Local(Id),
    /// A name the title index may answer for; resolved whole, by the resolver.
    Nominal,
    /// The path the link names, resolved against its document, normalized.
    Path(PathBuf),
}

impl Pre {
    /// Classify `link`, written in the document at `source`. `nominal` is
    /// whether the resolver would ask the title index about it at all: a body
    /// image never is.
    pub(crate) fn of(settings: &ReadSettings, source: &Path, link: &Link, nominal: bool) -> Self {
        if link.is_external() {
            return Pre::External;
        }
        if link.is_same_document() {
            return Pre::SameDocument;
        }
        match link.id_ref() {
            Some(IdRef::Local(id)) => return Pre::Local(id),
            // Qualified with this workspace's own name, it is local.
            Some(IdRef::Foreign { workspace, id }) => {
                if !settings.workspace_id.is_empty() && workspace == settings.workspace_id {
                    return Pre::Local(id);
                }
                return Pre::Foreign { workspace, id };
            }
            Some(IdRef::Malformed) => return Pre::Malformed,
            None => {}
        }
        if nominal && title::is_alias_shaped(link.addressed_target()) {
            return Pre::Nominal;
        }
        Pre::Path(link::resolve(source, &link.target))
    }
}

/// Where the document at `path`'s spanning links lead, as far as the link
/// alone says — what a tree prefetch reads next. A nominal link is taken as
/// the path it spells: the prefetch guesses, and a wrong guess is one failed
/// read.
#[cfg(all(feature = "parallel", not(target_family = "wasm")))]
pub(crate) fn spanning_children(settings: &ReadSettings, path: &Path, doc: &Document) -> Vec<Pre> {
    let Some(spanning) = settings.relations.spanning_relation() else {
        return Vec::new();
    };
    doc.meta
        .get(spanning)
        .map(crate::meta::Value::link_strings)
        .unwrap_or_default()
        .iter()
        .map(|raw| Pre::of(settings, path, &Link::parse(raw), false))
        .collect()
}

/// One link, prepared: where it was found, the link, its [`Pre`], and whether
/// its target is alias-shaped (what decides that the walk builds the title
/// index before resolving it).
#[derive(Debug, Clone)]
pub(crate) struct PreparedLink<S> {
    pub(crate) site: S,
    pub(crate) link: Link,
    pub(crate) pre: Pre,
    pub(crate) alias_shaped: bool,
}

/// A document's links, prepared for the walk. See the module docs.
#[derive(Debug)]
pub(crate) struct Prepared {
    /// Every frontmatter link, in [`frontmatter_links`](super::Graph::frontmatter_links)'
    /// order.
    pub(crate) front: Vec<PreparedLink<FrontmatterLink>>,
    /// Every body link, in [`link::scan_body_links`]' order.
    pub(crate) body: Vec<PreparedLink<BodyLink>>,
    /// The links in the spanning relation's inverse field (`part_of`), which
    /// the walk asks of a child to see whether it names its parent back.
    pub(crate) inverse: Vec<PreparedLink<()>>,
}

/// Prepare the document at `path`, whose body holds `body`.
pub(crate) fn prepare(
    settings: &ReadSettings,
    path: &Path,
    doc: &Document,
    body: &[BodyLink],
) -> Prepared {
    let meta = fig::Value::from(&doc.meta);
    let prepared = |link: Link, nominal: bool| {
        let pre = Pre::of(settings, path, &link, nominal);
        (link, pre)
    };
    let front = super::frontmatter_links_in(settings, &meta)
        .into_iter()
        .map(|site| {
            let link = Link::parse(&site.raw);
            let alias_shaped = title::is_alias_shaped(&link.target);
            let (link, pre) = prepared(link, true);
            PreparedLink {
                site,
                link,
                pre,
                alias_shaped,
            }
        })
        .collect();
    let body = body
        .iter()
        .map(|found| {
            let nominal = !found.image;
            PreparedLink {
                site: found.clone(),
                link: found.link.clone(),
                pre: Pre::of(settings, path, &found.link, nominal),
                alias_shaped: nominal && title::is_alias_shaped(&found.link.target),
            }
        })
        .collect();
    let inverse_field = settings.relations.spanning_relation().and_then(|spanning| {
        settings
            .relations
            .relations()
            .iter()
            .find(|r| r.name == spanning)
            .and_then(|r| r.inverse.clone())
    });
    let inverse = inverse_field
        .and_then(|field| meta.get(field.as_str()).map(crate::meta::link_strings))
        .unwrap_or_default()
        .iter()
        .map(|raw| {
            let link = Link::parse(raw);
            let alias_shaped = title::is_alias_shaped(&link.target);
            let (link, pre) = prepared(link, true);
            PreparedLink {
                site: (),
                link,
                pre,
                alias_shaped,
            }
        })
        .collect();
    Prepared {
        front,
        body,
        inverse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relation::RelationSet;

    fn settings(workspace_id: &str) -> ReadSettings {
        ReadSettings {
            relations: RelationSet::diaryx(),
            workspace_id: workspace_id.to_string(),
            ..ReadSettings::default()
        }
    }

    #[test]
    fn a_link_is_classified_as_the_resolvers_would_begin_to() {
        let s = settings("home");
        let src = Path::new("notes/a.md");
        let pre = |raw: &str, nominal| Pre::of(&s, src, &Link::parse(raw), nominal);
        assert_eq!(pre("https://x.org/a", true), Pre::External);
        assert_eq!(pre("#3", true), Pre::SameDocument);
        assert_eq!(
            pre("[t](b.md)", true),
            Pre::Path(PathBuf::from("notes/b.md"))
        );
        assert_eq!(pre("[t](/b.md#2)", true), Pre::Path(PathBuf::from("b.md")));
        assert_eq!(pre("[[My Page]]", true), Pre::Nominal);
        assert_eq!(
            pre("[[My Page]]", false),
            Pre::Path(PathBuf::from("notes/My Page"))
        );
        assert!(matches!(pre("id:home/abc1234", true), Pre::Local(_)));
        assert!(matches!(pre("id:away/abc1234", true), Pre::Foreign { .. }));
        assert_eq!(pre("id:", true), Pre::Malformed);
    }
}
