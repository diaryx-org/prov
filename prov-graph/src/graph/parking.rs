//! Parking — the directories a graph's walks never index.
//!
//! A workspace has interiors that are not its documents: another tool's store
//! it declared out of scope, the events and blobs of a retired history store,
//! a retired recycle bin's items. A walk still reaches whatever links reach,
//! but it never *indexes titles* inside one of these, so `[[Some Note]]`
//! cannot resolve to an old revision or a binned copy, and a walk that has to
//! fall back to a full title scan does not read a thousand revision documents
//! to throw their titles away.
//!
//! *Which* directories those are is the workspace's statement, not this
//! crate's: the graph is told, through [`ReadSettings::parking`], and does not
//! know what a history store looks like. What it does own is applying the
//! answer to every walk it makes — so a caller holding only the graph
//! (`prov_views::documents`, an export plan, a frontend) gets the same bound
//! the workspace's own walks get, without having to know there is one.
//!
//! [`ReadSettings::parking`]: super::ReadSettings::parking

use std::path::{Path, PathBuf};

use super::{Graph, Target};
use crate::error::Result;
use crate::fs::ReadStorage;
use crate::index::IdIndex;
use crate::link::{self, Link};

/// The directories a graph's walks never index titles inside.
///
/// Empty by default: a bare graph parks nothing, exactly as before the type
/// existed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parking {
    /// Workspace-relative directories parked outright, whatever a walk starts
    /// from — a workspace's declared `out_of_scope`.
    pub dirs: Vec<PathBuf>,
    /// Stores a walk's start document may point at, each parking directories
    /// inside itself.
    pub stores: Vec<ParkedStore>,
}

/// A store a root document may point at, and the directories inside it that
/// are parked while it does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParkedStore {
    /// The pointer relations that may name the store, in precedence order:
    /// the first one the root declares (and that resolves to a workspace
    /// path) names it, and the rest are not consulted. That is how a current spelling (`deletions`, which parks
    /// nothing) shadows a legacy one (`recycle_bin`, which parks `items/`).
    pub pointers: Vec<StorePointer>,
}

/// One relation that may name a [`ParkedStore`], and what it parks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorePointer {
    /// The pointer relation on the root, naming the store's index document.
    pub relation: String,
    /// Directories parked under the store — relative to the directory of its
    /// index document.
    pub parks: Vec<PathBuf>,
}

impl Parking {
    /// Whether this parks nothing at all.
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty() && self.stores.is_empty()
    }
}

impl<FS: ReadStorage, Ix: IdIndex> Graph<FS, Ix> {
    /// Every directory parked for a walk from `root_doc`: the declared
    /// [`Parking::dirs`], then the parked directories of each
    /// [`ParkedStore`] the root points at.
    ///
    /// Reads `root_doc` only when a store is configured, and fails when that
    /// read does. The walks themselves are forgiving (a walk from a missing
    /// start renders a missing node, it does not fail), so they fall back to
    /// the declared directories alone; a caller that wants the error asks
    /// here.
    pub async fn parked_dirs(&self, root_doc: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
        let parking = &self.settings.parking;
        let mut dirs = parking.dirs.clone();
        if parking.stores.is_empty() {
            return Ok(dirs);
        }
        let root_doc = link::normalize(root_doc);
        let (_, doc) = self.load(&root_doc).await?;
        for store in &parking.stores {
            for pointer in &store.pointers {
                let Some(raw) = doc
                    .meta
                    .get(&pointer.relation)
                    .map(crate::meta::Value::link_strings)
                    .and_then(|targets| targets.into_iter().next())
                else {
                    continue;
                };
                // A pointer that names nothing in the workspace (an external
                // URL, an unregistered id) names no store, and the next
                // spelling is tried as if it were absent.
                let Target::Path(index) = self.resolve_link(&root_doc, &Link::parse(&raw)) else {
                    continue;
                };
                let store_dir = index.parent().unwrap_or(Path::new("")).to_path_buf();
                dirs.extend(pointer.parks.iter().map(|sub| store_dir.join(sub)));
                break;
            }
        }
        Ok(dirs)
    }

    /// The parking a walk from `start` applies when its caller named none:
    /// [`parked_dirs`](Self::parked_dirs), or the declared directories alone
    /// when `start` will not load — the walk reports that itself.
    pub(crate) async fn walk_parking(&self, start: &Path) -> Vec<PathBuf> {
        match self.parked_dirs(start).await {
            Ok(dirs) => dirs,
            Err(_) => self.settings.parking.dirs.clone(),
        }
    }

    /// `parked` with the declared [`Parking::dirs`] added — what every walk
    /// told its parked directories explicitly still owes the workspace's
    /// declaration.
    pub(crate) fn with_declared(&self, parked: &[PathBuf]) -> Vec<PathBuf> {
        let mut out = parked.to_vec();
        for dir in &self.settings.parking.dirs {
            if !out.contains(dir) {
                out.push(dir.clone());
            }
        }
        out
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::exec::block_on;
    use crate::fs::StdFs;
    use crate::graph::{NodeKind, ReadSettings};
    use crate::index::NoIndex;

    use prov_testkit::write;

    /// A graph told its parking applies it to the plain `tree` walk: a name
    /// that also titles a document in a declared directory, or under a store
    /// the root points at, still names the one document a reader can reach.
    #[test]
    fn a_plain_tree_walk_does_not_resolve_a_name_into_a_parked_directory() {
        let dir = prov_testkit::scratch("parking", "tree");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nbin: bin/index.md\ncontents:\n- '[[A]]'\n- '[[B]]'\n---\n",
        );
        write(&dir, "a.md", "---\ntitle: A\npart_of: index.md\n---\n");
        write(&dir, "b.md", "---\ntitle: B\npart_of: index.md\n---\n");
        write(&dir, "vendor/a.md", "---\ntitle: A\n---\n");
        write(&dir, "bin/index.md", "---\ntitle: Bin\n---\n");
        write(&dir, "bin/items/b.md", "---\ntitle: B\n---\n");

        let kinds = |graph: &Graph<StdFs, NoIndex>| {
            let root = block_on(graph.tree("index.md")).unwrap();
            root.children
                .iter()
                .map(|c| c.kind.clone())
                .collect::<Vec<_>>()
        };
        let bare = Graph::new(StdFs, &dir, NoIndex, ReadSettings::default());
        assert!(
            kinds(&bare).iter().all(|k| *k != NodeKind::Doc),
            "unparked, each name is ambiguous"
        );

        let settings = ReadSettings {
            parking: Parking {
                dirs: vec![PathBuf::from("vendor")],
                stores: vec![ParkedStore {
                    pointers: vec![StorePointer {
                        relation: "bin".into(),
                        parks: vec![PathBuf::from("items")],
                    }],
                }],
            },
            ..ReadSettings::default()
        };
        let parked = Graph::new(StdFs, &dir, NoIndex, settings);
        assert_eq!(kinds(&parked), [NodeKind::Doc, NodeKind::Doc]);
        assert_eq!(
            block_on(parked.parked_dirs("index.md")).unwrap(),
            [PathBuf::from("vendor"), PathBuf::from("bin/items")]
        );
    }
}
