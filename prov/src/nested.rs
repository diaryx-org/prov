//! Workspaces nested inside this one, as a [`PeerResolver`] that answers first.
//!
//! A library can hold a copy of a text somebody else wrote as a workspace of
//! its own — its own node, root, relations and `workspace_id` — in a directory
//! of the library's folder. The library refers into it as
//! `id:<workspace>/<id>`, and the graph stops there as it does at any foreign
//! reference: where `<workspace>` is, is the host's to say.
//!
//! A peer file or a recent-libraries list says it per device, which is why
//! such a link resolves on one machine and nowhere else. A nested workspace's
//! directory is a fact about the *library*, not the device: it travels with
//! the folder. So the map here is read off the folder — every nested
//! workspace the walk finds ([`Graph::nested_workspaces`]), by the name its own
//! node declares — and a host consults it **before** its per-device map
//! ([`NestedPeers::before`]), nearest first.
//!
//! A name is unique only within the library it is resolved in. Two nested
//! workspaces declaring one name are an ambiguity, and [`NestedPeers`] refuses
//! it rather than guessing — including refusing to let a fallback answer for
//! the name, since the fallback's answer would be a guess too.
//!
//! [`Graph::nested_workspaces`]: prov_graph::graph::Graph::nested_workspaces

use std::collections::BTreeMap;
use std::path::PathBuf;

use prov_graph::error::Result;
use prov_graph::index::IdIndex;
use prov_graph::{Id, PeerLocation, PeerLookup, PeerResolver};
use prov_store::fs::Storage;

use crate::discovery::{Discovery, discover};
use crate::workspace::Workspace;

/// Every named workspace nested inside one workspace, by the name each
/// declares, with its absolute root directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NestedPeers {
    by_name: BTreeMap<String, Vec<PathBuf>>,
}

impl NestedPeers {
    /// A map built from `(name, root directory)` pairs — what
    /// [`Workspace::nested_peers`] reads off the folder, or a host's cache of
    /// it.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, PathBuf)>) -> Self {
        let mut by_name: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for (name, root) in pairs {
            let roots = by_name.entry(name).or_default();
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        for roots in by_name.values_mut() {
            roots.sort();
        }
        Self { by_name }
    }

    /// Every `(name, root directory)` pair, by name — for a host to cache.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &PathBuf)> {
        self.by_name
            .iter()
            .flat_map(|(name, roots)| roots.iter().map(move |root| (name.as_str(), root)))
    }

    /// Every name two or more nested workspaces declare, with their roots.
    pub fn ambiguous(&self) -> impl Iterator<Item = (&str, &[PathBuf])> {
        self.by_name
            .iter()
            .filter(|(_, roots)| roots.len() > 1)
            .map(|(name, roots)| (name.as_str(), roots.as_slice()))
    }

    /// Whether no named workspace is nested here.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// This map, consulted ahead of `fallback`: a name nested here is answered
    /// here, and only a name nothing here declares is asked of `fallback`.
    pub fn before<R: PeerResolver>(&self, fallback: R) -> Nearest<'_, R> {
        Nearest {
            nested: self,
            fallback,
        }
    }

    fn roots(&self, workspace: &str) -> Option<&[PathBuf]> {
        self.by_name.get(workspace).map(Vec::as_slice)
    }
}

impl PeerResolver for NestedPeers {
    /// Confirmed where exactly one nested workspace declares the name — it was
    /// read off that workspace's own node, which is the confirmation. A name
    /// declared twice answers [`Unknown`](PeerLookup::Unknown): there is no
    /// one location to give, and picking either would be a guess.
    fn locate(&self, workspace: &str) -> PeerLookup {
        match self.roots(workspace) {
            Some([root]) => {
                PeerLookup::confirm(workspace, PeerLocation::Path(root.clone()), workspace)
            }
            _ => PeerLookup::Unknown,
        }
    }
}

/// A [`NestedPeers`] in front of a host's own resolver. See
/// [`NestedPeers::before`].
#[derive(Debug, Clone)]
pub struct Nearest<'a, R> {
    nested: &'a NestedPeers,
    fallback: R,
}

impl<R: PeerResolver> PeerResolver for Nearest<'_, R> {
    fn locate(&self, workspace: &str) -> PeerLookup {
        match self.nested.roots(workspace) {
            Some(_) => self.nested.locate(workspace),
            None => self.fallback.locate(workspace),
        }
    }

    fn locate_document(&self, workspace: &str, id: &Id) -> Option<PeerLocation> {
        match self.nested.roots(workspace) {
            Some(_) => None,
            None => self.fallback.locate_document(workspace, id),
        }
    }
}

impl<FS: Storage, Id, Ix: IdIndex> Workspace<FS, Id, Ix> {
    /// Every named workspace nested inside this one, read off the folder.
    ///
    /// Each directory [`nested_workspaces`] finds is discovered as a workspace
    /// of its own, and kept when that workspace is rooted there and declares
    /// a name. An anonymous one names nothing a reference could spell, and one
    /// that will not open is left out: it is a [`check`] finding, not a route.
    ///
    /// [`nested_workspaces`]: prov_graph::graph::Graph::nested_workspaces
    /// [`check`]: crate::validate
    pub async fn nested_peers(&self) -> Result<NestedPeers> {
        let mut pairs = Vec::new();
        for rel in self.graph().nested_workspaces().await? {
            let at = self.root().join(&rel);
            let Ok(Discovery::Found(found)) = discover(&self.fs(), &at).await else {
                continue;
            };
            if prov_graph::link::normalize(&found.root_dir) != prov_graph::link::normalize(&at) {
                continue;
            }
            let name = found.config.workspace_id;
            if name.is_empty() {
                continue;
            }
            pairs.push((name, at));
        }
        Ok(NestedPeers::from_pairs(pairs))
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use std::path::Path;

    use prov_graph::NoPeers;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_store::index::FileIndex;

    use super::*;
    use prov_testkit::write;

    fn library(tag: &str) -> PathBuf {
        let dir = prov_testkit::scratch("nested", tag);
        write(&dir, "prov.yaml", "workspace_id: library\n");
        write(&dir, "index.md", "---\ntitle: Home\n---\n");
        write(
            &dir,
            "shelf/book/prov.yaml",
            "workspace_id: book\nroot: README.md\n",
        );
        write(&dir, "shelf/book/README.md", "---\ntitle: The Book\n---\n");
        dir
    }

    fn peers(dir: &Path) -> NestedPeers {
        let ws: Workspace<StdFs, crate::identity::NoIdentity, FileIndex> =
            Workspace::builder(StdFs)
                .root(dir)
                .index(FileIndex::new(fig::Format::Yaml))
                .build();
        block_on(ws.nested_peers()).unwrap()
    }

    #[test]
    fn a_nested_workspace_is_found_by_the_name_it_declares() {
        let dir = library("one");
        let peers = peers(&dir);

        assert_eq!(
            peers.locate("book"),
            PeerLookup::Confirmed(PeerLocation::Path(dir.join("shelf/book")))
        );
        assert_eq!(peers.locate("library"), PeerLookup::Unknown);
        assert_eq!(peers.ambiguous().count(), 0);
    }

    /// Two copies declaring one name in one library are refused, and the
    /// fallback is not asked to break the tie either.
    #[test]
    fn a_name_two_nested_workspaces_declare_is_refused_not_guessed() {
        let dir = library("two");
        write(
            &dir,
            "attic/book/prov.yaml",
            "workspace_id: book\nroot: README.md\n",
        );
        write(
            &dir,
            "attic/book/README.md",
            "---\ntitle: An Older Copy\n---\n",
        );
        let peers = peers(&dir);

        assert_eq!(peers.locate("book"), PeerLookup::Unknown);
        let ambiguous: Vec<_> = peers.ambiguous().collect();
        assert_eq!(
            ambiguous,
            [(
                "book",
                &[dir.join("attic/book"), dir.join("shelf/book")][..]
            )]
        );

        struct Everywhere;
        impl PeerResolver for Everywhere {
            fn locate(&self, workspace: &str) -> PeerLookup {
                PeerLookup::confirm(
                    workspace,
                    PeerLocation::Path("/elsewhere".into()),
                    workspace,
                )
            }
        }
        assert_eq!(peers.before(Everywhere).locate("book"), PeerLookup::Unknown);
        assert!(
            peers
                .before(Everywhere)
                .locate("notes")
                .followable()
                .is_some()
        );
        assert!(peers.before(NoPeers).locate("notes").is_unknown());
    }
}
