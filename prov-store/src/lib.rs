//! # prov-store
//!
//! The write surface over a [prov](https://docs.rs/prov) workspace: the half of
//! each storage port that changes something.
//!
//! ## What this crate is for
//!
//! [`prov_graph`] is the read core — it declares [`ReadStorage`] and
//! [`IdIndex`], and every traversal in it is generic over exactly those two.
//! This crate declares their other halves — [`Storage`] and [`IndexStore`] —
//! plus the metadata [`editor`](edit) that rewrites a document's frontmatter in
//! place.
//!
//! The two live in separate crates so that the read core's guarantee is
//! structural rather than editorial. A consumer that must not modify a
//! workspace — a language server, a static renderer, a browser viewer — depends
//! on `prov-graph` alone and *cannot* write, because the vocabulary for writing
//! is not in its dependency graph at all. Not a feature flag it might have left
//! on, not a convention review has to police: the functions do not exist.
//!
//! ## The shape of it
//!
//! - [`fs`] — [`Storage`], the write half of the filesystem port, and the
//!   durability vocabulary a backend answers with ([`Capabilities`],
//!   [`Durability`], [`SyncGuarantee`]). Also [`InMemoryFs`], a writable
//!   in-process backend for tests and sandboxes.
//! - [`edit`] — format-preserving, comment-preserving edits to a document's
//!   embedded metadata, whatever carries it.
//! - [`index`] — [`IndexStore`], the [`Rebase`] seam a pending change set
//!   answers through, and the two concrete registries ([`InMemoryIndex`] and
//!   the registry-document-backed [`FileIndex`]).
//!
//! The crash-atomic staging that drives these — the change set and its
//! write-ahead journal — is `prov-transaction`, a layer up.
//!
//! [`ReadStorage`]: prov_graph::fs::ReadStorage
//! [`IdIndex`]: prov_graph::index::IdIndex

// `edit` synthesizes a metadata block for a document that has none, and picks
// the archetype by format feature — so at least one backend must be compiled
// in here for the same reason it must be in `prov-graph`.
#[cfg(not(any(
    feature = "yaml",
    feature = "json",
    feature = "toml",
    feature = "fig-lang"
)))]
compile_error!(
    "prov-store needs at least one metadata-format feature enabled: \
     `yaml` (the default), `json`, `toml`, or `fig-lang`. \
     You have disabled the default feature without selecting a replacement."
);

pub mod edit;
pub mod fs;
pub mod index;

pub use edit::MetaEditor;
pub use fs::{Capabilities, Durability, InMemoryFs, Storage, SyncGuarantee};
pub use index::{FileIndex, IndexStore, InMemoryIndex, Rebase};
