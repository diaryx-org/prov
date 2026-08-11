//! Link resolution — turning one declared target (a path, an `id:`
//! reference, or a nominal `[[alias]]`) into a [`Target`] against a
//! workspace. See the module doc at [`crate::graph`] for how this sits beside
//! the census and the read primitive in [`load`](super::load).

use std::path::{Path, PathBuf};

use super::Graph;
use crate::identity;
use crate::index::IdIndex;
use crate::link::{self, IdRef, Link};
use crate::title::{self, TitleIndex, TitleMatch};

/// The resolution of one link target against a workspace: a path, an ID the
/// registry does not currently resolve, or an off-workspace reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A (normalized, workspace-relative) path.
    Path(PathBuf),
    /// An `id:<id>` reference with no live registry entry — unknown,
    /// tombstoned, or the workspace has no registry at all.
    UnresolvedId(identity::Id),
    /// A nominal (alias) reference whose name several documents claim, so it
    /// cannot be resolved to one. The `String` is the name as written.
    AmbiguousAlias(String),
    /// A URL or mail address — never resolved against the workspace and never
    /// rewritten by moves.
    External,
    /// An `id:<workspace>/<id>` reference naming a document in *another*
    /// workspace — carried, never rewritten, and never reported broken.
    ///
    /// prov stops here on purpose. Resolving this would require a map from a
    /// workspace name to a location, and that map is a property of the device
    /// doing the reading, not of the archive being read: the same reference
    /// resolves to a directory on one machine, a URL on another, and nothing at
    /// all on a third. So the library reports *what was named* and leaves
    /// *where it lives* to the host — `prov-cli` keeps a device-local peer map,
    /// diaryx resolves through its published ARK permalinks.
    ///
    /// A reference qualified with this workspace's own
    /// [`workspace_id`](Graph::workspace_id) is **not** foreign: it is
    /// resolved locally through the registry, so a document carrying one keeps
    /// working when it is copied into the workspace it names.
    Foreign {
        /// The workspace qualifier, exactly as written.
        workspace: String,
        /// The id within that workspace, exactly as written — never
        /// check-verified here (that workspace owns its id space, and may not
        /// be a prov workspace at all).
        id: identity::Id,
    },
}

impl<FS, Ix: IdIndex> Graph<FS, Ix> {
    /// Resolve `link` (declared in the document at `doc`) to a workspace target,
    /// without nominal (alias) resolution — path and `id:` targets only. Use
    /// [`resolve_link_with`](Self::resolve_link_with) when a [`TitleIndex`] is
    /// available and `[[My File]]`-style aliases should resolve.
    pub fn resolve_link(&self, doc: &Path, link: &Link) -> Target {
        self.resolve_link_with(doc, link, None)
    }

    /// Resolve `link` to a workspace target. Path targets resolve relative to
    /// `doc`'s directory; an `id:<id>` target resolves through the registry (the
    /// location-independent path that stays valid across moves); an
    /// alias-shaped target (a bare name) resolves through `titles` when one is
    /// supplied — `Unique` to its path, `Ambiguous` to
    /// [`Target::AmbiguousAlias`], and `Unknown` falling through to a path (so a
    /// nominal link to nothing surfaces as a missing/broken path, exactly as
    /// before aliases existed). With `titles` `None`, alias resolution is off
    /// and this is the pure path/id resolver.
    pub fn resolve_link_with(
        &self,
        doc: &Path,
        link: &Link,
        titles: Option<&TitleIndex>,
    ) -> Target {
        if link.is_external() {
            return Target::External;
        }
        // A reference qualified with this workspace's own name *is* local — the
        // registry that issued the id is the one in hand. That equivalence is
        // what makes a qualified reference survive being copied into the
        // workspace it names, instead of going inert at the boundary.
        let id = match link.id_ref() {
            Some(IdRef::Local(id)) => Some(id),
            Some(IdRef::Foreign { workspace, id }) => {
                if !self.workspace_id().is_empty() && workspace == self.workspace_id() {
                    Some(id)
                } else {
                    return Target::Foreign { workspace, id };
                }
            }
            // Malformed: the author wrote `id:`, so this is a broken id
            // reference, not a filename that happens to contain a colon.
            Some(IdRef::Malformed) => {
                return Target::UnresolvedId(identity::Id(link.target.clone()));
            }
            None => None,
        };
        if let Some(id) = id {
            return match self.index().resolve(&id) {
                Some(path) => Target::Path(link::normalize(path)),
                None => Target::UnresolvedId(id),
            };
        }
        if let Some(titles) = titles
            && title::is_alias_shaped(&link.target)
        {
            match titles.resolve(&link.target) {
                TitleMatch::Unique(path) => return Target::Path(link::normalize(path)),
                TitleMatch::Ambiguous(_) => return Target::AmbiguousAlias(link.target.clone()),
                // Unknown: fall through — a bare name with nothing behind it is
                // treated as a path, so it reads as missing like any dead link.
                TitleMatch::Unknown => {}
            }
        }
        Target::Path(link::resolve(doc, &link.target))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::graph::ReadSettings;
    use crate::index::IdIndex;

    #[derive(Clone)]
    struct DummyFs;

    /// A registry holding exactly one registration. The concrete stores live in
    /// `prov-store`, on the write side of the port — what resolution needs from
    /// an index is only the two lookups below, so the fixture supplies only
    /// those rather than reaching across the split for a store it would then
    /// have to mutate to populate.
    struct OneEntry(identity::Id, PathBuf);

    impl IdIndex for OneEntry {
        fn resolve(&self, id: &identity::Id) -> Option<PathBuf> {
            (*id == self.0).then(|| self.1.clone())
        }

        fn id_for_path(&self, path: &Path) -> Option<identity::Id> {
            (path == self.1).then(|| self.0.clone())
        }
    }

    /// A graph named `notes` whose registry resolves `ajp7eq`.
    fn named_ws(name: &str) -> Graph<DummyFs, OneEntry> {
        Graph::new(
            DummyFs,
            "vault",
            OneEntry(identity::Id("ajp7eq".into()), PathBuf::from("note.md")),
            ReadSettings {
                workspace_id: name.to_string(),
                ..ReadSettings::default()
            },
        )
    }

    #[test]
    fn a_reference_to_another_workspace_resolves_to_foreign() {
        let ws = named_ws("notes");
        let link = Link::parse("id:diaryx/xk4m2p");
        assert_eq!(
            ws.resolve_link(Path::new("a.md"), &link),
            Target::Foreign {
                workspace: "diaryx".into(),
                id: identity::Id("xk4m2p".into()),
            }
        );
    }

    #[test]
    fn a_reference_qualified_with_our_own_name_is_local() {
        // The invariant with teeth: a document written elsewhere as
        // `id:notes/ajp7eq` keeps working once it is copied *into* `notes`,
        // instead of going inert at the boundary.
        let ws = named_ws("notes");
        assert_eq!(
            ws.resolve_link(Path::new("a.md"), &Link::parse("id:notes/ajp7eq")),
            Target::Path(PathBuf::from("note.md"))
        );
        // And it agrees with the unqualified spelling of the same reference.
        assert_eq!(
            ws.resolve_link(Path::new("a.md"), &Link::parse("id:ajp7eq")),
            ws.resolve_link(Path::new("a.md"), &Link::parse("id:notes/ajp7eq"))
        );
    }

    #[test]
    fn an_anonymous_workspace_treats_every_qualifier_as_foreign() {
        // With no name of its own, a workspace has nothing to compare against —
        // so it must not guess that `id:notes/…` means itself.
        let ws = named_ws("");
        assert_eq!(
            ws.resolve_link(Path::new("a.md"), &Link::parse("id:notes/ajp7eq")),
            Target::Foreign {
                workspace: "notes".into(),
                id: identity::Id("ajp7eq".into()),
            }
        );
    }

    #[test]
    fn a_malformed_id_reference_is_not_reread_as_a_path() {
        // `id:a/b/c` is a broken id reference, not a filename. Resolving it as a
        // path would turn a typo into a plausible-looking dead path link.
        let ws = named_ws("notes");
        assert!(matches!(
            ws.resolve_link(Path::new("a.md"), &Link::parse("id:a/b/c")),
            Target::UnresolvedId(_)
        ));
    }
}
