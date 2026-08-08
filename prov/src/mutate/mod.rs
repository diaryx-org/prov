//! Mutation with link maintenance — the crate's hard, valuable half.
//!
//! Creating, moving, and deleting a document are never single-file operations
//! in a linked workspace: the spanning relation and its inverse live in *other*
//! documents, and every touched link must keep pointing at the truth. Each op
//! here computes the full set of affected documents, edits their metadata with
//! fig's comment-preserving [`fig::Embed`] editor (byte-minimal diffs, fence
//! style and format untouched, labels on `[label](path)` links kept), and only
//! then touches the filesystem.
//!
//! ## Identity is additive here (DESIGN §4)
//!
//! Everything below operates on paths and never *requires* an ID. When a
//! registry is present, each op additionally keeps it true — create registers
//! (if the policy's `on_create` fires), rename updates `id → path`, delete
//! tombstones — and a `colophon:<id>` entry in another document's metadata is
//! deliberately **not** rewritten by a move: the registry update is what keeps
//! it resolving, which is the entire point of linking by ID. With
//! [`crate::identity::NoIdentity`]/[`crate::index::NoIndex`] these hooks
//! monomorphize to nothing.
//!
//! The vocabulary is never hardcoded: the spanning relation and its inverse
//! come from the workspace's [`crate::relation::RelationSet`].
//!
//! ## Writes are staged, not issued
//!
//! Every op here computes its edits and stages them into a
//! [`ChangeSet`](crate::change::ChangeSet), which lands as one unit — documents
//! and, when the op moved an ID, the registry with them. No error can leave the
//! workspace half-linked, and behind the write-ahead journal
//! ([`crate::journal`]) no crash can either: an interrupted op resolves to the
//! workspace fully before it or fully after it. Ops remain documents-only: no
//! directory moves.
//!
//! ## Where the code lives
//!
//! The module is split by *what a reader is after*: one file per verb, each an
//! `impl Workspace` block.
//!
//! - `create` — a new document authored under a parent, in the parent's shape.
//! - `adopt`, `reparent` — an *existing* document linked under a parent:
//!   additively (`adopt`), or in place of the parent it already claims
//!   (`reparent`).
//! - `rename` — a document's path changes and every link that touched it
//!   follows; `retitle` — its title changes and every inbound *label* follows.
//! - `delete` — the hard delete; `recycle` — the recoverable one, with
//!   `restore` and `empty_bin` beside it.
//! - `separate` — one combined document split into a metadata node and a body
//!   file, and `combine` back.
//! - `duplicate` — a shallow copy as a fresh sibling.
//! - `convert` — the re-spellings that move no document: a link's style, and a
//!   metadata block's language or embedding shape.
//!
//! `maintain` holds the plumbing those verbs share: walking the spanning
//! relation (up to the root, down a subtree, along one entry), resolving the
//! two files of a separated pair, and retargeting every inbound reference to a
//! document that moved.
//!
//! Tests sit in each file's own `mod tests`, as elsewhere in the crate. The
//! fixtures they share — a seeded workspace, a linked tree, a backend that
//! fails the nth write — are in `support`, which no sibling could own without
//! every other one reaching across for it.

mod adopt;
mod convert;
mod create;
mod delete;
mod duplicate;
pub(crate) mod maintain;
mod recycle;
mod rename;
mod reparent;
mod retitle;
mod separate;

pub use create::Created;

#[cfg(all(test, feature = "yaml"))]
mod support;

// Laws over the verbs *together* — the sequences no fixture would think to
// write. Not any one verb's file, because it is not any one verb's claim.
#[cfg(all(test, feature = "yaml"))]
mod properties;

// Three properties the ops rest on rather than implement — the id map they
// maintain, the config document they read policy from, and the registration
// hook they fire — exercised here, through the same fixtures, because
// `workspace.rs` (where all three live) has no filesystem-backed test surface
// of its own.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::support::*;
    use crate::identity::Trigger;
    use std::path::{Path, PathBuf};

    #[test]
    fn scan_ids_rebuilds_the_id_map_from_frontmatter() {
        // Frontmatter-only storage: each document carries its own `id`; a flat
        // scan reconstructs the id→path map with no registry document.
        let dir = tempdir("scan-ids");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nid: aaaaaaa\n---\nbody\n",
        );
        write(
            &dir,
            "sub/child.md",
            "---\ntitle: Child\nid: bbbbbbb\n---\nbody\n",
        );
        // A document with no `id` is simply absent from the map, not an error.
        write(&dir, "sub/plain.md", "---\ntitle: Plain\n---\nbody\n");

        let mut ids = block_on(ws(&dir).scan_ids()).unwrap();
        ids.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        assert_eq!(
            ids,
            vec![
                (
                    crate::identity::Id("aaaaaaa".into()),
                    PathBuf::from("index.md")
                ),
                (
                    crate::identity::Id("bbbbbbb".into()),
                    PathBuf::from("sub/child.md")
                ),
            ]
        );
    }

    #[test]
    fn config_pointer_resolves_and_reads_a_setting() {
        // Workspace policy lives in a config document the root links via the
        // `config` relation — the registry's reachability move, for config.
        let dir = tempdir("config");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nconfig: prov.yaml\n---\n",
        );
        write(
            &dir,
            "prov.yaml",
            "title: prov config\npart_of: index.md\nlink_format: plain_relative\n",
        );
        let ws = ws(&dir);
        assert_eq!(
            block_on(ws.config_path(Path::new("index.md"))).unwrap(),
            Some(PathBuf::from("prov.yaml"))
        );
        let value = block_on(ws.config_get(Path::new("index.md"), "link_format")).unwrap();
        assert_eq!(
            value.and_then(|v| v.as_str().map(str::to_owned)),
            Some("plain_relative".into())
        );
        // An unset key falls through to None (caller uses its default).
        assert!(
            block_on(ws.config_get(Path::new("index.md"), "missing"))
                .unwrap()
                .is_none()
        );
        // No pointer at all → no config document.
        write(&dir, "bare.md", "---\ntitle: Bare\n---\n");
        assert!(
            block_on(ws.config_path(Path::new("bare.md")))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn register_is_idempotent_and_policy_gated() {
        let dir = tempdir("id-register");
        write(&dir, "a.md", "---\ntitle: A\n---\n");

        let mut w = id_ws(&dir);
        let first = block_on(w.register(Path::new("a.md"), Trigger::Link)).unwrap();
        let again = block_on(w.register(Path::new("a.md"), Trigger::Link)).unwrap();
        assert_eq!(first, again, "idempotent");
        assert!(crate::identity::verify(first.as_str()));

        // Lazy policy: `Create` does not fire.
        write(&dir, "b.md", "---\ntitle: B\n---\n");
        let err = block_on(w.register(Path::new("b.md"), Trigger::Create)).unwrap_err();
        assert!(err.to_string().contains("does not register"), "{err}");
    }
}
