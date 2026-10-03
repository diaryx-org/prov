//! What a change set did to the tree, told to whoever asked as it lands.
//!
//! Every write a [`Workspace`](crate::Workspace) makes passes through
//! [`apply_set`](crate::Workspace::apply_set): a rename, a retitle, a move, a
//! save, a confirmation, each is one [`ChangeSet`] landed through the
//! write-ahead journal, all of it or none of it. So that is where a consumer
//! can learn exactly which files an operation touched, without surveying the
//! tree to find out.
//!
//! A [`Landing`] hook, installed with
//! [`set_landing`](crate::Workspace::set_landing), is told each set that
//! lands, after it has landed, as a [`Landed`]: the paths written, moved and
//! removed. A set that fails tells it nothing, because nothing happened. The
//! hook is called synchronously, from inside the write, with the workspace
//! still borrowed: it should note what it is told and return — a history that
//! records each operation as a revision of its own does the recording after
//! the verb returns, over the paths it noted.
//!
//! prov keeps none of this. A workspace with no hook pays for nothing but the
//! check that there is none.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::{ChangeSet, FileOp};

/// What one change set did to the tree. Paths are root-relative, as the set
/// staged them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Landed {
    /// Files written: created, replaced, or given new metadata (an execute
    /// bit, a link's target).
    pub written: Vec<PathBuf>,
    /// Files moved, from where to where.
    pub moved: Vec<(PathBuf, PathBuf)>,
    /// Files removed.
    pub removed: Vec<PathBuf>,
}

impl Landed {
    /// What `set` does, op by op, in the order it stages them.
    pub fn of(set: &ChangeSet) -> Self {
        let mut landed = Landed::default();
        for op in set.ops() {
            match op {
                FileOp::Write { path, .. }
                | FileOp::CopyFrom { path, .. }
                | FileOp::SetExecutable { path, .. }
                | FileOp::SetLink { path, .. } => landed.written.push(path.clone()),
                FileOp::Rename { from, to } => landed.moved.push((from.clone(), to.clone())),
                FileOp::Remove { path } => landed.removed.push(path.clone()),
            }
        }
        landed
    }

    /// Every path the set touched, at either end of a move.
    pub fn paths(&self) -> BTreeSet<PathBuf> {
        self.written
            .iter()
            .chain(self.removed.iter())
            .chain(self.moved.iter().flat_map(|(from, to)| [from, to]))
            .cloned()
            .collect()
    }

    /// Whether the set did nothing.
    pub fn is_empty(&self) -> bool {
        self.written.is_empty() && self.moved.is_empty() && self.removed.is_empty()
    }
}

/// Who to tell when a change set lands. See the [module](self) docs.
pub trait Landing: Send + Sync {
    /// `landed` is now on disk.
    fn landed(&self, landed: &Landed);
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_testkit::{read, write};

    use super::*;
    use crate::Workspace;

    /// A hook that keeps what it is told.
    #[derive(Default)]
    struct Kept(Mutex<Vec<Landed>>);

    impl Landing for Kept {
        fn landed(&self, landed: &Landed) {
            self.0.lock().unwrap().push(landed.clone());
        }
    }

    fn tree(tag: &str) -> std::path::PathBuf {
        let dir = prov_testkit::scratch("landing", tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\ncontents:\n- mid.md\n- other.md\n---\n",
        );
        write(&dir, "mid.md", "---\ntitle: Mid\npart_of: index.md\n---\n");
        write(
            &dir,
            "other.md",
            "---\ntitle: Other\npart_of: index.md\n---\nSee [Mid](mid.md).\n",
        );
        dir
    }

    /// A rename lands as one set: the move, and every document whose links
    /// followed it — exactly those, and nothing the hook would have to find
    /// by surveying the tree.
    #[test]
    fn a_rename_is_told_as_the_move_and_the_links_that_followed_it() {
        let dir = tree("rename");
        let kept = Arc::new(Kept::default());
        let mut ws = Workspace::builder(StdFs).root(&dir).build();
        ws.set_landing(kept.clone());
        block_on(ws.rename(Path::new("mid.md"), Path::new("sub/mid.md"))).unwrap();
        assert!(read(&dir, "other.md").contains("sub/mid.md"));

        let told = kept.0.lock().unwrap();
        assert_eq!(told.len(), 1, "one verb, one set: {told:?}");
        let landed = &told[0];
        assert_eq!(
            landed.moved,
            [(PathBuf::from("mid.md"), PathBuf::from("sub/mid.md"))]
        );
        let written: BTreeSet<_> = landed.written.iter().cloned().collect();
        assert!(
            written.contains(Path::new("index.md")) && written.contains(Path::new("other.md")),
            "the documents whose links followed: {landed:?}"
        );
        assert!(landed.removed.is_empty());
        assert!(landed.paths().contains(Path::new("mid.md")));
        assert!(landed.paths().contains(Path::new("sub/mid.md")));
    }

    /// A delete is told as a removal, and a workspace with no hook is told
    /// nothing — there is nobody to tell.
    #[test]
    fn a_delete_is_a_removal_and_no_hook_hears_nothing() {
        let dir = tree("delete");
        let kept = Arc::new(Kept::default());
        let mut ws = Workspace::builder(StdFs)
            .root(&dir)
            .landing(kept.clone())
            .build();
        block_on(ws.delete(Path::new("other.md"), false)).unwrap();
        let told = kept.0.lock().unwrap();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].removed.contains(&PathBuf::from("other.md")));

        let mut quiet = Workspace::builder(StdFs).root(&dir).build();
        assert!(quiet.landing().is_none());
        block_on(quiet.rename(Path::new("mid.md"), Path::new("moved.md"))).unwrap();
    }

    /// A clone writes to the same tree, so whoever asked about the tree's
    /// writes hears about the clone's too.
    #[test]
    fn a_clone_keeps_the_hook() {
        let dir = tree("clone");
        let kept = Arc::new(Kept::default());
        let mut ws = Workspace::builder(StdFs).root(&dir).build();
        ws.set_landing(kept.clone());
        let mut twin = ws.clone();
        block_on(twin.rename(Path::new("mid.md"), Path::new("twin.md"))).unwrap();
        assert_eq!(kept.0.lock().unwrap().len(), 1);
    }
}
