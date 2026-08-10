//! # prov
//!
//! A *self-describing plaintext workspace*: a set of documents whose structure
//! lives in the documents' own embedded metadata (frontmatter), not in the
//! filesystem layout or an app-private sidecar folder.
//!
//! The name is the point. A *prov* is the note in which a book describes its
//! own making — the type, the paper, the press. A prov workspace is one you
//! can hand to any tool and it explains itself: follow the links in the metadata
//! and the whole structure unfolds, with a distinguished root that describes the
//! whole.
//!
//! ## The shape of the abstraction
//!
//! - **Documents** are plaintext files with an embedded metadata block
//!   ([`document::Document`]).
//! - **Relations** are named links declared in that metadata
//!   ([`relation::RelationSet`]). *Which* fields are links is configurable
//!   (`contents`/`part_of`, `links`, or your own vocabulary); the mechanism is
//!   not. Exactly one relation may be marked **spanning** — the single-parent
//!   tree that gives the workspace its self-describing discovery spine. Every
//!   other relation may be many-to-many, so the tree is a backbone, never a
//!   ceiling.
//! - **Identity** is a strictly-additive layer ([`identity`], [`index`]). The
//!   graph, traversal, and (eventually) mutation operate on *paths* and never
//!   require an ID. Turn identity off and it compiles out; turn it on and IDs
//!   are minted only when something durably refers to a document.
//!
//! ## Status
//!
//! Early extraction from `diaryx_core`. The pure layers — embedded-metadata
//! parsing ([`meta`]), document splitting, and relation extraction — are real
//! and tested. The filesystem-driven scan/traversal/mutation engine ports next;
//! its seams ([`workspace::Workspace`], [`identity::IdentityPolicy`],
//! [`index::IndexStore`]) are staked out here so nothing diaryx-specific leaks
//! into the eventual public API.

// At least one embedded-metadata format backend must be compiled in, otherwise
// prov can neither parse nor serialize any metadata. The format features
// (`yaml`, `json`, `fig-lang`) forward to the matching `fig` parser — see
// `Cargo.toml`.
#[cfg(not(any(feature = "yaml", feature = "json", feature = "fig-lang")))]
compile_error!(
    "prov needs at least one metadata-format feature enabled: \
     `yaml` (the default), `json`, or `fig-lang`. \
     You have disabled the default feature without selecting a replacement."
);

pub mod about;
pub mod attach;
pub mod change;
pub mod config;
pub mod discovery;
pub mod fixity;
#[cfg(test)]
mod fs_faults;
pub mod history;
pub mod identity;
pub mod intake;
pub mod journal;
pub mod mutate;
pub mod remedy;
pub mod route;
pub mod textdist;
pub mod validate;
pub mod vocabulary;
pub mod workspace;

/// The read core, re-exported whole.
///
/// `prov` is `prov-graph` plus the verbs. A consumer that needs both should
/// depend on `prov` alone and reach everything through here; a consumer that
/// only traverses can depend on `prov-graph` directly and link none of the
/// mutation, history, or config machinery.
pub use prov_graph;
pub use prov_graph::{
    Addressing, Backlink, BodyLink, Capabilities, Cardinality, CensusEntry, Collision,
    ContentFormat, DirEntry, Document, Durability, Edge, EmbedStyle, EmbedType, Error, ExtKind,
    FileIndex, FileType, Format, Graph, Id, IdIndex, IdStorage, InMemoryFs, InMemoryIndex,
    IndexStore, Link, LinkSite, LinkStyle, Mapping, MetaCarrier, Metadata, NoIndex, Node, NodeKind,
    Notation, PathStyle, ReadScope, ReadSettings, ReadStorage, Rebase, ReferenceStyle, Relation,
    RelationSet, Resolution, Result, StdFs, Storage, StructuralFact, SyncGuarantee, Target,
    TitleIndex, TitleMatch, TreeOptions, Value, Walk, Wikilink, Wrapper, block_on, code_spans,
    embed_carrier, embed_style_of, escapes_root, format_link, is_opaque_payload, path_to_title,
    reachable_set, render_html, require_whole_file,
};
/// The read core's modules, re-exported at their original paths so `prov`'s
/// public API is exactly what it was before the split.
pub use prov_graph::{
    content, document, edit, error, exec, fs, graph, index, link, memo, meta, relation, title,
};

pub use about::AboutContext;
pub use change::{ChangeSet, FileOp};
pub use config::{
    About, ConfigIssue, ConfigIssueKind, FIELD_TYPES, FieldSpec, Fixity, History, OpenClosed,
    RelationDef, RelationStyleConfig, WorkspaceConfig, diagnose, field_type_as_config_str,
    field_type_from_config_str, is_valid_workspace_id, metadata_format_from_str,
    metadata_format_str, spec_ahead,
};
pub use discovery::{Discovered, Discovery, discover};
/// The field-type vocabulary a `fields.<name>.type` declaration is spelled in,
/// re-exported so a consumer can name types without depending on `fig-schema`
/// (or, for [`ExtKind`], on `fig`) directly — and so neither can drift to a
/// different version than the one prov resolves against.
pub use fig_schema::FieldType;
pub use fixity::FixityCache;
pub use history::{
    Captured, Conflict, Disposition, Event, FileEntry, Forgotten, Latest, Presence, Pruned,
    RestoreOp, RestorePlan, Retention, Scope, StoreLocation, Subject, Summary, Version,
};
pub use identity::{IdentityPolicy, Minter, NoIdentity, Registration, Trigger};
pub use intake::{Adoption, PlanOutcome, StructurePlan, SynthNode};
pub use journal::{Recovered, recover};
pub use mutate::Created;
pub use remedy::{Fix, Remedy, RemedyKind, Warrant};
pub use route::{Layout, RoutePlan};
pub use validate::{CheckDiff, Finding};
pub use vocabulary::{Term, Vocabulary};
pub use workspace::{Settings, Workspace, WorkspaceBuilder};
