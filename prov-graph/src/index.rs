//! Index — where stable IDs and (later) the materialized graph live.
//!
//! An index fuses two natures (DESIGN §5): the **authoritative** id↔path
//! registry — not rebuildable from the documents — and (to come) the
//! **derived** resolution cache and adjacency index, which are. Keeping it
//! behind a trait is deliberate: a sidecar file, an in-memory map, or a
//! sync-backed store are all valid homes.
//!
//! Only the query half is here — [`IdIndex`], the lookups link resolution
//! needs. Everything that *changes* a registration (`IndexStore`, the
//! `Rebase` seam, the in-memory and registry-document stores) is
//! `prov-store`'s `index` module, for the same reason the write half of
//! [`fs`](crate::fs) is: a read-only consumer must not merely decline to write,
//! it must have nothing to write with.
//!
//! ## Tombstones — IDs are forever
//!
//! DESIGN's open question #1 ("does the registry ever need to survive without
//! its documents?") is answered **yes, minimally**: deleting a document leaves
//! a *tombstone* — the ID stops resolving but is never forgotten, so it can
//! never be reminted to mean something else. A dangling `prov:` reference
//! then stays *diagnosable* (validation can say "that document was deleted")
//! instead of becoming a silent re-resolution hazard. [`is_known`] is the
//! question that tells the two apart.
//!
//! [`is_known`]: IdIndex::is_known

use std::path::{Path, PathBuf};

use crate::identity::Id;

/// A registration that would displace one the index already holds.
///
/// The index is a **bijection**: one id names one path, one path carries one id.
/// Registering across an existing entry breaks that, and in one of two
/// directions — worth telling apart, because what the user has to do about them
/// differs.
///
/// Both are ordinary under sync. `id_storage` defaults to `both`, so a document's
/// id travels *in its own frontmatter*: a transport can land a copy of a document
/// under a new name, and now two files spell one id with the registry able to
/// name only one of them. The registry cannot arbitrate that — only the author
/// can — so an operation that would resolve it silently refuses instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Collision {
    /// The id already resolves to a *different* live document. Registering would
    /// take the id away from a document whose frontmatter still spells it.
    Id {
        /// The id being registered.
        id: Id,
        /// The document that currently holds it.
        held_by: PathBuf,
    },
    /// The path already carries a *different* id. Registering would drop that id
    /// out of the registry while the document on disk still spells it — turning a
    /// live id into an unregistered one.
    Path {
        /// The path being registered.
        path: PathBuf,
        /// The id it currently carries.
        held: Id,
    },
}

impl std::fmt::Display for Collision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Collision::Id { id, held_by } => {
                write!(f, "{id} is already registered to {}", held_by.display())
            }
            Collision::Path { path, held } => {
                write!(f, "{} already carries {held}", path.display())
            }
        }
    }
}

/// The query half of an ID index: the three lookups link resolution needs, and
/// no way to change what is stored.
///
/// This is the trait [`crate::graph`] is generic over, and it is the whole of
/// what the read core asks of a registry — `id:` resolution
/// ([`resolve`](IdIndex::resolve)), the reverse lookup a census entry is tagged
/// with ([`id_for_path`](IdIndex::id_for_path)), and the tombstone question that
/// distinguishes "never existed" from "retired"
/// ([`is_known`](IdIndex::is_known)).
///
/// Split out of `prov-store`'s `IndexStore` for the same reason
/// [`ReadStorage`](crate::fs::ReadStorage) is split out of that crate's
/// `Storage`: a read-only consumer must be able to depend on traversal without
/// linking the staging machinery, and `IndexStore`'s staging half is not merely
/// unused by the read core — it is *stated in write vocabulary*, down to
/// `rebase`, which only a pending mutation has anything to say to.
pub trait IdIndex {
    /// Resolve an ID to its current path. `None` for unknown *and* tombstoned
    /// IDs — use [`is_known`](IdIndex::is_known) to tell them apart.
    fn resolve(&self, id: &Id) -> Option<PathBuf>;

    /// The ID currently assigned to `path`, if any.
    fn id_for_path(&self, path: &Path) -> Option<Id>;

    /// Whether `id` has *ever* been issued — live or tombstoned. This is the
    /// mint-with-rejection predicate: a fresh ID must be `!is_known`.
    fn is_known(&self, id: &Id) -> bool {
        self.resolve(id).is_some()
    }
}

/// No index — identity-off workspaces. Registers nothing, resolves nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoIndex;

impl IdIndex for NoIndex {
    fn resolve(&self, _id: &Id) -> Option<PathBuf> {
        None
    }
    fn id_for_path(&self, _path: &Path) -> Option<Id> {
        None
    }
}
