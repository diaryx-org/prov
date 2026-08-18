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
// (`yaml`, `json`, `toml`, `fig-lang`) forward to the matching `fig` parser —
// see `Cargo.toml`.
#[cfg(not(any(
    feature = "yaml",
    feature = "json",
    feature = "toml",
    feature = "fig-lang"
)))]
compile_error!(
    "prov needs at least one metadata-format feature enabled: \
     `yaml` (the default), `json`, `toml`, or `fig-lang`. \
     You have disabled the default feature without selecting a replacement."
);

pub mod about;
pub mod attach;
pub use prov_config as config;
pub mod discovery;
/// Content fixity policy, digests, and the device-local cache.
pub use prov_fixity as fixity;
#[cfg(test)]
mod fs_faults;
pub mod history;
pub mod identity;
pub mod intake;
pub mod manifest;
pub mod mutate;
pub mod remedy;
pub mod route;
pub mod validate;
pub use prov_config::vocabulary;
pub mod workspace;

/// The read core, re-exported whole.
///
/// `prov` is `prov-graph` plus the verbs. A consumer that needs both should
/// depend on `prov` alone and reach everything through here; a consumer that
/// only traverses can depend on `prov-graph` directly and link none of the
/// mutation, history, or config machinery.
pub use prov_graph;
pub use prov_graph::{
    Addressing, Backlink, BodyLink, Cardinality, CensusEntry, Collision, ContentFormat, DirEntry,
    Document, Edge, EmbedStyle, EmbedType, Error, ExtKind, FileType, Format, Graph, Id, IdIndex,
    IdStorage, Link, LinkSite, LinkStyle, Manifest, ManifestEntry, Mapping, MetaCarrier, Metadata,
    NoIndex, NoPeers, Node, NodeKind, Notation, PathStyle, PeerLocation, PeerLookup, PeerResolver,
    ReadScope, ReadSettings, ReadStorage, ReferenceStyle, Relation, RelationSet, Resolution,
    Result, StdFs, StructuralFact, Target, TitleIndex, TitleMatch, TreeOptions, Unconfirmed, Value,
    Walk, Wikilink, Wrapper, block_on, code_spans, embed_carrier, embed_style_of, escapes_root,
    format_link, is_opaque_payload, path_to_title, reachable_set, render_html, require_whole_file,
};
/// The read core's modules, re-exported at their original paths so `prov`'s
/// public API is exactly what it was before the split.
pub use prov_graph::{
    content, document, error, exec, graph, link, memo, meta, peer, relation, title,
};
/// Metadata editing, at the path it had before the write surface moved out of
/// the read core into `prov-store`.
pub use prov_store::edit;
pub use prov_store::{
    Capabilities, Durability, FileIndex, InMemoryFs, InMemoryIndex, IndexStore, Rebase, Storage,
    SyncGuarantee,
};

/// The filesystem port — both halves.
///
/// The read surface ([`ReadStorage`](prov_graph::fs::ReadStorage) and the types
/// it answers with) is `prov-graph`'s; the write surface
/// ([`Storage`](prov_store::fs::Storage) and the durability vocabulary) is
/// `prov-store`'s. They live in separate crates so a read-only consumer can
/// depend on the first without linking the second, and are rejoined here
/// because `prov` is the layer that does both.
pub mod fs {
    pub use prov_graph::fs::{DirEntry, FileType, Metadata, ReadStorage, StdFs};
    pub use prov_store::fs::{
        Capabilities, Durability, InMemoryFs, Storage, SyncGuarantee, memory,
    };
}

/// The ID index — both halves, split across two crates for the same reason
/// [`fs`] is.
pub mod index {
    pub use prov_graph::index::{Collision, IdIndex, NoIndex};
    pub use prov_store::index::{FileIndex, InMemoryIndex, IndexStore, Rebase};
}

pub use about::AboutContext;
/// Transaction primitives, retained at their original paths for compatibility.
pub mod change {
    pub use prov_transaction::change::{ChangeSet, FileOp};
}
/// Journal recovery, retained at its original path for compatibility.
pub mod journal {
    pub use prov_transaction::journal::{JOURNAL_NAME, Recovered, recover};

    #[allow(unused_imports)]
    pub(crate) use prov_transaction::journal::{decode, encode, is_journal_path};
}
pub use config::{
    About, ConfigIssue, ConfigIssueKind, FIELD_TYPES, FieldSpec, Fixity, History, OpenClosed,
    RelationDef, RelationStyleConfig, WorkspaceConfig, diagnose, field_type_as_config_str,
    field_type_from_config_str, is_valid_workspace_id, metadata_format_from_str,
    metadata_format_str, spec_ahead,
};
pub use discovery::{Discovered, Discovery, discover};
/// Declarative views over the workspace — the `views:` config axis, the
/// traversal that selects the documents one covers, and the pure grouping over
/// what it selected.
///
/// Re-exported at prov's own path so a consumer that already depends on prov
/// need not add a second crate to read the views its config carries, and so the
/// two cannot resolve to different versions of `ViewSpec`. A consumer that
/// wants *only* views — a renderer, a browser view — should depend on
/// `prov-views` directly instead: it reaches nothing that can write.
pub mod views {
    pub use prov_views::{
        CONDITION_KEYS, Condition, Error, Grain, Group, Grouping, Row, RowSet, Selection,
        VIEW_KEYS, VIEWS_KEY, ViewIssue, ViewIssueKind, ViewSpec, diagnose_view, diagnose_views,
        group, select, views_from,
    };
}
/// Named, closed-by-default document sets that may leave the workspace — the
/// `exports:` config axis, and the plan that composes a gate with a view.
///
/// Re-exported at prov's own path for the same reasons [`views`] is. prov
/// itself never consumes a plan: what an [`ExportPlan`](exports::ExportPlan)
/// feeds — a publish step, a copy-out, an OCFL export — lives downstream, and
/// the invariant (an export is a subset of what its gate admits) lives in
/// `prov-exports` with the planner.
pub mod exports {
    pub use prov_exports::{
        EXPORT_KEYS, EXPORTS_KEY, Error, ExportDoc, ExportIssue, ExportIssueKind, ExportPlan,
        ExportSpec, GATE_KEYS, Gate, Withheld, compose, diagnose_export, diagnose_exports,
        exports_from, plan,
    };
}
/// The field-type vocabulary a `fields.<name>.type` declaration is spelled in,
/// re-exported so a consumer can name types without depending on `fig-schema`
/// (or, for [`ExtKind`], on `fig`) directly — and so neither can drift to a
/// different version than the one prov resolves against.
pub use fig_schema::FieldType;
pub use fixity::FixityCache;
pub use history::{
    Captured, Conflict, Disposition, Event, FileEntry, Forgotten, HistoryIssue, HistoryStore,
    Latest, Presence, Pruned, RestoreOp, RestorePlan, Retention, Retrieved, Scope, StoreLocation,
    Subject, Summary, Version,
};
pub use identity::{
    IdentityPolicy, Minter, NoIdentity, Registration, Trigger, WORKSPACE_NAME_LEN,
    mint_workspace_id,
};
pub use intake::{Adoption, PlanOutcome, StructurePlan, SynthNode};
pub use manifest::{ManifestStatus, ManifestUpdate};
pub use mutate::Created;
pub use prov_exports::ExportSpec;
pub use prov_transaction::{ChangeSet, FileOp};
pub use prov_transaction::{Recovered, recover};
pub use prov_views::ViewSpec;
pub use remedy::{Fix, Remedy, RemedyKind, Warrant};
pub use route::{Layout, RoutePlan};
pub use validate::{CheckDiff, Finding};
pub use vocabulary::{Term, Vocabulary};
pub use workspace::{Settings, Workspace, WorkspaceBuilder};
