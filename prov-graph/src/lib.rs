//! # prov-graph
//!
//! The read core of a [prov](https://docs.rs/prov) workspace: plaintext
//! documents, the links declared in their own embedded metadata, and the
//! traversal over them.
//!
//! ## What this crate is for
//!
//! A prov workspace describes itself. Follow the links in a document's
//! frontmatter and body and the whole structure unfolds — no index to trust
//! instead of the documents, no sidecar folder that has to be kept in step.
//! This crate is that unfolding, and *only* that.
//!
//! Everything here reads. The filesystem port it asks for
//! ([`fs::ReadStorage`]) has no method that writes a byte; the id index it asks
//! for ([`index::IdIndex`]) has no method that changes a registration. Nor is
//! the vocabulary for writing merely unused — it is *absent*, declared a layer
//! up in `prov-store` instead. So a consumer that must not modify a workspace —
//! a language server, a static renderer, a browser viewer — can depend on this
//! crate and be *unable* to, rather than merely intending not to. That is the
//! whole reason the split exists, and it is why the write halves are not here
//! behind a feature flag someone could leave switched on.
//!
//! The write surface is `prov-store`: `Storage`, the metadata editor, and the
//! `IndexStore` registries. The verbs are `prov`: creating, renaming, deleting,
//! attaching, the change/journal machinery that makes a mutation crash-atomic,
//! the config layer, the validation and repair passes.
//! `prov` owns one [`Graph`] and forwards every read to it, so the two are the
//! same traversal — not a reimplementation that can drift.
//!
//! `prov-views` is what that promise looks like taken up: a whole view engine —
//! parse a declared view, resolve its scope by walking the spanning relation,
//! group the documents it reaches — built on this crate and nothing else, and
//! therefore unable to modify a byte of what it reads.
//!
//! ## The shape of it
//!
//! - [`Document`] — a plaintext file split into its embedded metadata block and
//!   its body.
//! - [`relation::RelationSet`] — which metadata fields are links. Exactly one
//!   may be **spanning**: the single-parent tree that gives a workspace its
//!   discovery spine. Every other relation may be many-to-many, so the tree is a
//!   backbone, never a ceiling.
//! - [`Graph`] — a root, a [`fs::ReadStorage`], an [`index::IdIndex`], and the
//!   [`graph::ReadSettings`] that say how links are spelled. Its two walks are
//!   the [`census`](Graph::census) (every forward link, flat, each tagged with
//!   where it is written and how it resolves) and the [`tree`](Graph::tree)
//!   (the spanning relation only, as a materialized outline).
//!
//! The census is ground truth. Reachability, the backlinks map, and prov's own
//! validation findings are all views over it, and any stored index heals
//! *toward* it, never the reverse.

// At least one embedded-metadata format backend must be compiled in, otherwise
// nothing here can parse a document at all. The format features (`yaml`,
// `json`, `toml`, `fig-lang`) forward to the matching `fig` parser.
#[cfg(not(any(
    feature = "yaml",
    feature = "json",
    feature = "toml",
    feature = "fig-lang"
)))]
compile_error!(
    "prov-graph needs at least one metadata-format feature enabled: \
     `yaml` (the default), `json`, `toml`, or `fig-lang`. \
     You have disabled the default feature without selecting a replacement."
);

pub mod content;
pub mod document;
pub mod error;
pub mod exec;
pub mod fixity;
pub mod fs;
pub mod graph;
pub mod identity;
pub mod index;
pub mod link;
pub mod manifest;
pub mod memo;
pub mod meta;
pub mod peer;
pub mod relation;
pub mod title;

pub use content::{ContentFormat, code_spans, render_html};
pub use document::{
    Body, Document, EmbedStyle, EmbedType, MetaCarrier, embed_carrier, embed_style_of,
    is_opaque_payload, require_whole_file,
};
pub use error::{Error, Result};
pub use exec::block_on;
pub use fig::ExtKind;
pub use fig::Format;
pub use fixity::Fixity;
pub use fs::{DirEntry, FileType, Metadata, ReadStorage, StdFs};
pub use graph::{
    Backlink, CensusEntry, Graph, LinkSite, Node, NodeKind, ReadSettings, Resolution,
    StructuralFact, Target, TreeOptions, Walk, reachable_set,
};
pub use identity::{Id, IdStorage};
pub use index::{Collision, IdIndex, NoIndex};
pub use link::{
    Addressing, BodyLink, Link, LinkStyle, Notation, PathStyle, ReferenceStyle, Wikilink, Wrapper,
    escapes_root, format_link, is_valid_workspace_id, path_to_title,
};
pub use manifest::{Manifest, ManifestEntry, manifest_sibling};
pub use memo::ReadScope;
pub use meta::{Mapping, Value};
pub use peer::{NoPeers, PeerLocation, PeerLookup, PeerResolver, Unconfirmed};
pub use relation::{Cardinality, Edge, Relation, RelationSet};
pub use title::{TitleIndex, TitleMatch};

/// The body-prose parser, re-exported whole.
///
/// [`content`] uses twig to answer prov's own two questions — render a body to
/// HTML ([`render_html`]) and find the spans a parser calls code
/// ([`code_spans`]) — and both hand back plain strings and offsets. That is the
/// whole of what prov needs, and for a long time it was the whole of what
/// anyone could reach: twig was an implementation detail with no path out.
///
/// It is re-exported because the consumers this crate was built for — a
/// language server, a static renderer, a browser viewer — need the *tree*, not
/// a rendering of it. A static site generator filtering `:::vis{...}` regions
/// by audience, or an editor addressing a node to splice it, is asking twig
/// questions prov has no opinion about and should not grow one about.
///
/// Without this they would depend on `twig-doc` directly, pin it themselves,
/// and resolve to a different [`twig::Document`] than the one [`content`]
/// parses with — two AST vocabularies in one build, disagreeing silently about
/// what a document is.
///
/// **This makes `twig-doc` a public dependency**, which is a real cost and the
/// reason it was not done sooner: twig's major version is now part of prov's
/// semver contract, so a twig 4 is a breaking change for prov whether or not
/// prov's own surface moves. Accepted deliberately — the alternative is not
/// "no coupling", it is the same coupling spelled separately by every
/// downstream crate and enforced by nobody.
///
/// ```
/// use prov_graph::twig::{Document, Format, MarkdownExtensions};
///
/// // What [`content`] cannot ask for: an opt-in extension. prov parses with
/// // defaults, so a consumer that needs directives reaches past it — and,
/// // through this re-export, reaches the same twig.
/// let directives = MarkdownExtensions { directives: true, ..Default::default() };
/// let mut doc = Document::parse_str_with(
///     ":::vis{.public}\nHello\n:::\n",
///     Format::Markdown,
///     directives,
/// )?;
/// // The directive's name becomes the element tag and its attributes ride
/// // along — which is also why a consumer publishing HTML unwraps these
/// // rather than rendering them.
/// let html = String::from_utf8(doc.render_html()?).unwrap();
/// assert!(html.contains("<vis class=\"public\">"), "{html}");
/// # Ok::<(), prov_graph::twig::Error>(())
/// ```
pub use twig;
