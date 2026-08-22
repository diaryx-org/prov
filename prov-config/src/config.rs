//! Workspace configuration — the typed policy a standalone/CLI workspace reads
//! from its **config document** (the `config`-relation target from the root,
//! DESIGN §6's reachability move applied to policy) and from its root's
//! `prov:` frontmatter block.
//!
//! Programmatic embedders never need this: they configure the [`Workspace`]
//! directly through the builder (`.link_style`, `.identity`, …), which is why
//! the type-level identity/index choice lives there. `WorkspaceConfig` is the
//! **data** shape that lets a workspace configure *itself* — so the same tool
//! serves a Diaryx-style vault and an Obsidian-style one purely by what the
//! config declares:
//!
//! - [`WorkspaceConfig::paths_only`] — path links, identity off (pure paths).
//! - [`WorkspaceConfig::stable_ids`] — stable IDs minted lazily (registry +
//!   backlinks), portable links for the path-based parts.
//!
//! The vocabulary (`docs/config-vocab.md`) is one namespace of keys with two
//! homes: nested under `prov:` in the root's frontmatter (the description
//! home) or at the top level of the dedicated config document (the policy home).
//! [`apply`](WorkspaceConfig::apply) reads either shape; unset keys keep their
//! default, and layering root block then config document gives the precedence
//! *config document > root `prov:` block > default*.
//!
//! [`Workspace`]: crate::workspace::Workspace

use std::collections::BTreeMap;

use fig::ExtKind;
use fig_schema::FieldType;

use crate::textdist::nearest;
use prov_exports::{ExportIssueKind, ExportSpec};
pub use prov_fixity::Fixity;
use prov_graph::content::ContentFormat;
use prov_graph::document::EmbedStyle;
use prov_graph::link::{Addressing, LinkStyle, Notation, PathStyle, ReferenceStyle};
use prov_graph::meta::{Mapping, Value};
use prov_graph::relation::{Cardinality, Relation, RelationSet};
use prov_identity::{Registration, Trigger};
use prov_views::{ViewIssueKind, ViewSpec};

/// Where a document's stable id is persisted. Defined in `prov-graph`, because
/// it is the one identity setting that changes what a link *resolves to* — a
/// reader has to know whether frontmatter is a place an id can be found.
pub use prov_graph::identity::IdStorage;

/// The config-vocabulary version stamped as `spec` and recognized on read — a
/// marker so a foreign tool (or a future prov) knows which vocabulary it is
/// looking at. Bumped only on an incompatible reshape.
pub const SPEC_VERSION: i64 = 1;

/// The root-frontmatter key under which workspace policy is nested. A root
/// document's frontmatter mixes structural links, identity, and user-owned
/// fields with the occasional policy setting; nesting policy under this one key
/// keeps the two apart, so config is unambiguous to read *and* to lint, and an
/// unrecognized *sibling* is never mistaken for a misspelled setting. The
/// dedicated config document needs no such wrapper — the whole document is policy
/// (`docs/config-vocab.md`, "The two homes").
pub const ROOT_CONFIG_KEY: &str = "prov";

/// A per-relation reference-style override, as declared in a config's
/// `relations` block. Each axis is optional and inherits the workspace default
/// ([`WorkspaceConfig::reference_style`]) when absent — so a block need only name
/// the axes it changes. This is the config form of
/// [`Relation::style`](prov_graph::relation::Relation::style), and what lets links
/// going "down" (`contents`) differ from links going "up" (`part_of`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelationStyleConfig {
    /// The notation override (`markdown` / `wikilink` / `bare`).
    pub notation: Option<Notation>,
    /// The path-resolution override (`root` / `relative`).
    pub path_style: Option<PathStyle>,
    /// The addressing override (`path` / `id` / `alias`).
    pub target: Option<Addressing>,
    /// The `id`-wikilink label override.
    pub label: Option<bool>,
}

/// A relation *definition* declared in a config's `relations` block — the
/// structural half of an entry, parallel to the reference-style half
/// ([`RelationStyleConfig`]). This is what makes a workspace's vocabulary
/// **self-describing** (DESIGN §1, the `prov/1` spec): a foreign reader learns
/// the graph — which fields are relations, their inverse, their cardinality —
/// from the document itself rather than assuming prov's `contents`/`part_of`
/// preset. Each field is optional; a `relations` entry may carry only style, only
/// definition, or both.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationDef {
    /// How many targets the field may hold (`one` / `many`). `None` leaves the
    /// relation's cardinality to whatever the built [`RelationSet`] defaults it to
    /// (`many`, the permissive choice) when this def creates the relation.
    pub cardinality: Option<Cardinality>,
    /// The reciprocal relation's field name, bidirectionally maintained.
    pub inverse: Option<String>,
    /// A free-form, human-facing gloss of what the relation means. prov never
    /// reads this back (DESIGN §2, tier 3) — it is documentation that travels with
    /// the data so a person reading the frontmatter learns the vocabulary too.
    pub means: Option<String>,
}

/// Whether a controlled `fields` vocabulary is *open* (folksonomy — unknown
/// values are allowed, only near-misses warn) or *closed* (every value must be a
/// known term; an unknown value is an error). See the `fields` block and
/// [`crate::vocabulary`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OpenClosed {
    /// Unknown values allowed; `check` warns only on a probable typo of a known
    /// term (casing/spelling drift).
    #[default]
    Open,
    /// Every value must resolve to a known term; an unknown value is a hard
    /// `check` finding. The right posture for a safety-critical vocabulary (a
    /// diaryx `audience`, where a typo is a disclosure bug).
    Closed,
}

impl OpenClosed {
    /// Parse the `values` config spelling; unknown → `None`.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }

    /// The `values` config spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

/// A field declaration — an entry in the `fields` block. It promotes a
/// frontmatter field (`tags`, `audience`, `created`) that prov would otherwise
/// merely carry (DESIGN §2, tier 3) into something prov and its frontends know
/// the shape of. Two independent things can be declared, and a field needs at
/// least one of them to be worth an entry:
///
/// - **A type** ([`ty`](Self::ty)) — what the value *is*. Pure data shape,
///   decidable from the value alone, so it is spelled in `fig-schema`'s
///   vocabulary rather than one prov invents.
/// - **A vocabulary** ([`vocabulary`](Self::vocabulary)) — which values are
///   *legal*, turning the field into a resolvable reference prov keeps
///   consistent: every value is checked against the vocabulary document the
///   pointer reaches.
///
/// They compose (a closed vocabulary of strings is both), but neither implies
/// the other: `created` is a date with no vocabulary, and a vocabulary field
/// needs no declared type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSpec {
    /// The type the field's values are expected to take, if declared. Drives
    /// type-directed parsing and widget choice in a frontend (a `date` field
    /// gets a date picker); prov itself carries it without interpreting it.
    pub ty: Option<FieldType>,
    /// Whether the value set is open (folksonomy) or closed (must be known).
    /// Meaningful only alongside a [`vocabulary`](Self::vocabulary).
    pub values: OpenClosed,
    /// The pointer (a link) to the vocabulary document listing this field's legal
    /// terms — resolved like the `registry`/`config` pointers (DESIGN §6). `None`
    /// for a field that declares a type but no controlled vocabulary.
    pub vocabulary: Option<String>,
    /// Whether each term is reified as its own node (rich: backlinks, a prose
    /// body, stable id) rather than a bare key in a flat registry. A hint to
    /// tooling; prov validates membership either way.
    pub reify: bool,
}

/// The config spellings of [`FieldType`], in the order a diagnostic offers them.
///
/// A deliberate subset of `fig-schema`'s type vocabulary: the kinds a *document
/// field* can meaningfully declare. `fig`'s remaining extended kinds
/// (`EnumLiteral`, `CharLiteral`, `NumberSpecial`) are artifacts of particular
/// serializations — ZON, JSON5 — rather than things a workspace declares about
/// its own metadata, so they get no spelling here.
pub const FIELD_TYPES: &[&str] = &[
    "str",
    "bool",
    "int",
    "float",
    "date",
    "datetime",
    "local-datetime",
    "time",
    "ref",
    "map",
    "seq",
];

/// Parse a `fields.<name>.type` spelling into a [`FieldType`]; unknown → `None`.
///
/// A free function rather than an inherent method because [`FieldType`] is
/// `fig-schema`'s type, not prov's — but the shape mirrors
/// [`OpenClosed::from_config_str`] and its siblings, since this is the same kind
/// of config-vocabulary translation.
///
/// The date/time spellings map onto `fig`'s extended scalars, which round-trip
/// as a format's *native* date where the format has one (a TOML `1979-05-27`
/// stays a date rather than becoming a quoted string) and as plain unquoted text
/// where it does not (YAML frontmatter, where the same value reads back as a
/// string — harmless, since a rule is matched by path, not by value type).
pub fn field_type_from_config_str(value: &str) -> Option<FieldType> {
    Some(match value {
        "str" => FieldType::Str,
        "bool" => FieldType::Bool,
        "int" => FieldType::Int,
        "float" => FieldType::Float,
        // An instant carrying its offset — the archivally honest default, and
        // what `updated:` stamps.
        "datetime" => FieldType::Extended(ExtKind::OffsetDateTime),
        "local-datetime" => FieldType::Extended(ExtKind::LocalDateTime),
        "date" => FieldType::Extended(ExtKind::LocalDate),
        "time" => FieldType::Extended(ExtKind::LocalTime),
        "ref" => FieldType::Ref,
        "map" => FieldType::Map,
        "seq" => FieldType::Seq,
        _ => return None,
    })
}

/// The `fields.<name>.type` spelling of a [`FieldType`], or `None` for a type
/// with no config spelling (see [`FIELD_TYPES`]) — such a type is dropped on
/// serialization rather than written as something that would not read back.
pub fn field_type_as_config_str(ty: FieldType) -> Option<&'static str> {
    Some(match ty {
        FieldType::Str => "str",
        FieldType::Bool => "bool",
        FieldType::Int => "int",
        FieldType::Float => "float",
        FieldType::Ref => "ref",
        FieldType::Map => "map",
        FieldType::Seq => "seq",
        FieldType::Extended(ExtKind::OffsetDateTime) => "datetime",
        FieldType::Extended(ExtKind::LocalDateTime) => "local-datetime",
        FieldType::Extended(ExtKind::LocalDate) => "date",
        FieldType::Extended(ExtKind::LocalTime) => "time",
        FieldType::Null | FieldType::Extended(_) => return None,
        // `FieldType` is `#[non_exhaustive]` upstream, so a version of
        // fig-schema newer than this one may name a type prov has no config
        // spelling for. That is the same case as `Null`: no spelling, so it is
        // dropped rather than written as something that would not read back.
        _ => return None,
    })
}

/// Whether the workspace maintains a **historica store's skiplist** — the
/// generated region of `history/skipped.txt` that scopes what `historica
/// record` takes to the workspace's reachable graph.
///
/// Recording itself is historica's, not prov's; what prov contributes is
/// which files are the workspace, and this axis says whether prov is asked to
/// keep the store's scoping current (`prov history-skips --write`).
///
/// Default **off**, unlike `recycle_bin`: a version-control store is ongoing
/// storage the user has not asked for. It is also the wrong tool when the
/// transport is **git**, which already stores every pre-image and reconciles
/// concurrent histories — a historica store earns its keep on Dropbox,
/// Syncthing, iCloud, a synced network share.
///
/// `off` gates *writing* only. Computing and showing the plan works
/// regardless: asking what the workspace fails to reach is a question about
/// the workspace, not about history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum History {
    /// The skiplist is not maintained (`off`, the default). `history-skips`
    /// still shows its plan; `--write` refuses.
    #[default]
    Off,
    /// The skiplist is rewritten when the user asks (`manual`) — `prov
    /// history-skips --write`, run by hand or by a pre-record script the user
    /// wires up themselves. prov does not run the recording, so there is no
    /// event for it to hook.
    Manual,
}

/// Whether the workspace generates **`about.md`** — a short prose page,
/// specialized against this workspace's own configuration, that tells a reader
/// with no prior knowledge how to read *this* directory.
///
/// The gap it closes is narrow and specific. A prov workspace already explains
/// its *structure* — the links are in the documents, visibly — but not its
/// *conventions*: what the links mean, how they are spelled, which files are in
/// the tree and which are not. Those live in the config, which is machine-facing
/// and assumes the reader already knows what its keys mean. So a person who
/// opens the directory with no prior knowledge cannot today learn to read it
/// *from* the directory; they must obtain `docs/spec.md`, which is a dependency
/// on an institution surviving — exactly the dependency the project refuses
/// everywhere else.
///
/// The page is **not** a vendored copy of the spec. It is the spec *specialized*
/// against this configuration: every rule resolved to a concrete fact, every
/// branch this workspace does not take deleted. Where the spec says "the block
/// is fenced by `---`, `;;;`, or ```` ```fig ````," the generated page says
/// "every file here opens with a `---` line." Nothing is lost operationally, and
/// the sentence is about *this directory* rather than about prov.
///
/// Default **on**, unlike [`History`]: it costs a few hundred bytes and one
/// file, and a workspace that explains itself to a stranger by default is the
/// whole thesis — making it opt-in concedes it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum About {
    /// No page is generated and the root declares no `about` pointer (`off`).
    Off,
    /// Generate the page describing the workspace's **structure** (`structure`,
    /// the default): the root and the spine; how a file is fenced; how a
    /// reference is written and what else is read; the relation vocabulary; what
    /// is machinery and not in the tree; the id, checksum and deletion
    /// conventions.
    #[default]
    Structure,
}

impl About {
    /// Whether a page is generated at all.
    pub fn generates(self) -> bool {
        matches!(self, About::Structure)
    }

    /// Parse the `about` config spelling; unknown → `None`.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "structure" => Some(Self::Structure),
            _ => None,
        }
    }

    /// The `about` config spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Structure => "structure",
        }
    }
}

impl History {
    /// Whether `history-skips --write` is permitted to rewrite the region.
    pub fn captures(self) -> bool {
        matches!(self, History::Manual)
    }

    /// Parse the `history` config spelling; unknown → `None`.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }

    /// The `history` config spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Manual => "manual",
        }
    }
}

/// The workspace-wide policy a config declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceConfig {
    /// When a document earns a stable ID — the identity registration triggers.
    pub identity: Registration,
    /// The default reference **notation** (`markdown` / `wikilink` / `bare`).
    /// Overridden per relation by [`Relation::style`](prov_graph::relation::Relation::style).
    pub notation: Notation,
    /// The default **path resolution** for path targets (`root` / `relative` /
    /// Ignored for id/alias targets.
    pub path_style: PathStyle,
    /// The default reference **addressing** (`path` / `id` / `alias`).
    pub reference_target: Addressing,
    /// Whether an id/alias reference carries a `|Title` label.
    pub reference_label: bool,
    /// Per-relation reference-style overrides, keyed by relation name — the
    /// config form of [`Relation::style`](prov_graph::relation::Relation::style).
    /// Each entry overlays the workspace default for that relation only, letting
    /// `contents` (down) and `part_of` (up) carry different styles. Empty means
    /// every relation inherits the default. Resolve with
    /// [`resolved_relation_styles`](Self::resolved_relation_styles).
    pub relation_styles: BTreeMap<String, RelationStyleConfig>,
    /// The name of the **spanning** relation — the single-parent containment tree
    /// that is the workspace's discovery spine (DESIGN §3). `None` leaves it to
    /// the built vocabulary's default. Declaring it in config is what lets a
    /// non-diaryx vocabulary name its own spine.
    pub spanning: Option<String>,
    /// Per-relation structural **definitions**, keyed by relation name — the
    /// self-describing half of the `relations` block (cardinality, inverse,
    /// human gloss). Empty means the workspace uses its built-in vocabulary
    /// (diaryx) unchanged. Consumed by [`relation_set`](Self::relation_set).
    pub relation_defs: BTreeMap<String, RelationDef>,
    /// Controlled-vocabulary field declarations, keyed by frontmatter field name
    /// (`tags`, `audience`). Empty means no field is controlled — every such
    /// field is ordinary carried content (DESIGN §2, tier 3).
    pub fields: BTreeMap<String, FieldSpec>,
    /// The views the workspace declares, in declaration order — the second way
    /// through the same documents the spine already holds ("the entries under
    /// `Daily`, by month"). Empty means the workspace declares none, which is
    /// not the same as having none to offer: a frontend is free to derive a
    /// lens from a `fields` declaration, and a *declared* view is the workspace
    /// overriding that.
    ///
    /// prov reads them and never acts on one. A view has no invariant to keep,
    /// so nothing in `check` can be violated by a wrong one — it is carried
    /// here so that every tool over the workspace reads the same views, rather
    /// than each app namespacing its own block and agreeing by convention.
    /// Executing one is `prov-views`.
    pub views: Vec<ViewSpec>,
    /// The exports the workspace declares, in declaration order — the named,
    /// closed-by-default sets that may *leave* it, each bounded by a gate and
    /// optionally arranged by one of [`views`](Self::views). Empty means
    /// nothing is declared exportable, which is the default state of a
    /// workspace and of every document in it.
    ///
    /// Carried here for the same reason `views` is — one axis every tool
    /// reads — but unlike a view an export *has* an invariant, and it lives
    /// with the planner in `prov-exports`: a plan's entries are a subset of
    /// what the gate admits, whatever the named view says.
    pub exports: Vec<ExportSpec>,
    /// Where a document's stable ID is persisted — registry, frontmatter shadow,
    /// or both (DESIGN §5). Independent of the `identity` trigger.
    pub id_storage: IdStorage,
    /// The metadata format new documents get when they inherit no parent block
    /// — a *default* for authoring, never a workspace constraint (§7).
    pub default_embed_format: fig::Format,
    /// How that metadata is *embedded* — delimiters, a fenced code block, an
    /// HTML island, or a separate sidecar. Together with `default_embed_format`
    /// it selects the carrier a fresh root/document is authored in; recorded so
    /// the workspace is self-describing about its embedding convention. Like
    /// `default_embed_format`, an authoring default rather than a constraint:
    /// existing documents keep whatever carrier they already have.
    pub embed_style: EmbedStyle,
    /// The body-prose grammar the workspace is authored in (Markdown/Djot/HTML)
    /// — the format `render` and code-aware link scanning assume, and the
    /// intended default for new documents.
    pub content_format: ContentFormat,
    /// Whether a `delete` moves the document to the **recycle bin** (recoverable)
    /// rather than destroying it. On by default — the safe posture for archival
    /// use, where a deletion should never be silently unrecoverable — and opt-out
    /// per workspace for those who genuinely want a hard delete as the default.
    pub recycle_bin: bool,
    /// How far content-checksum (fixity) coverage extends — attachments only (the
    /// default), attachments plus document bodies, or off.
    pub fixity: Fixity,
    /// Whether the workspace keeps a **history store** of captured pre-images —
    /// the safety net for structural damage an external sync transport introduces.
    /// Off by default; see [`History`].
    pub history: History,
    /// Whether the workspace generates **`about.md`**, the prose page that tells
    /// a stranger how to read this directory. On by default; see [`About`].
    pub about: About,
    /// The frontmatter field `prov edit` stamps with the current time when a
    /// document's content changes — the machine-maintained "last updated" field.
    /// Empty (the default) disables it. The *name* is yours (`updated`,
    /// `modified`, `lastmod`); the *value* is always machine-standard (RFC 3339
    /// UTC), because prov reads it back to know when to rewrite it. A
    /// human-friendly date is a *different*, user-owned field prov never
    /// touches (see DESIGN §2, "does prov read it back?").
    pub updated: String,
    /// What this workspace calls **itself** — the qualifier a cross-workspace
    /// reference (`id:<workspace>/<id>`) names it by. Empty (the default) means
    /// the workspace is anonymous: it can still *hold* foreign references, but
    /// no reference can be recognized as pointing back at it.
    ///
    /// This is the one piece of cross-workspace linking that is genuinely a fact
    /// about the archive, so it is the one piece that lives in its config. Where
    /// some *other* workspace can be found is a property of a device, not of
    /// this workspace, and deliberately has no config key — see
    /// [`Target::Foreign`](prov_graph::graph::Target::Foreign).
    ///
    /// Must be [well-formed](is_valid_workspace_id): a malformed value is
    /// reported by [`diagnose`] and ignored rather than half-honored.
    pub workspace_id: String,
}

/// Whether `name` is a usable workspace self-name.
///
/// Re-exported at the path it has always had, but *defined* beside the grammar
/// it is a constraint on: every clause of it is dictated by how an
/// `id:<workspace>/<id>` target parses, which is `prov-graph`'s business, not
/// policy this crate gets a say in.
pub use prov_graph::link::is_valid_workspace_id;

impl Default for WorkspaceConfig {
    /// The standalone default: portable markdown-root path links, identity
    /// available lazily (IDs minted only on a durable link-by-id or publish, §4),
    /// and path addressing (id-linking is opt-in).
    fn default() -> Self {
        Self {
            identity: Registration::LAZY,
            notation: Notation::Markdown,
            path_style: PathStyle::Root,
            reference_target: Addressing::Path,
            reference_label: false,
            relation_styles: BTreeMap::new(),
            spanning: None,
            relation_defs: BTreeMap::new(),
            fields: BTreeMap::new(),
            views: Vec::new(),
            exports: Vec::new(),
            id_storage: IdStorage::Frontmatter,
            default_embed_format: fig::Format::Yaml,
            embed_style: EmbedStyle::Delimited,
            content_format: ContentFormat::Markdown,
            recycle_bin: true,
            fixity: Fixity::Payloads,
            history: History::Off,
            about: About::Structure,
            updated: String::new(),
            workspace_id: String::new(),
        }
    }
}

impl WorkspaceConfig {
    /// Diaryx-style: path links, no identity — nothing mints an ID, so the
    /// workspace is addressed purely by path (the Adam's-Archive shape).
    pub fn paths_only() -> Self {
        Self {
            identity: Registration::OFF,
            id_storage: IdStorage::Registry,
            ..Self::default()
        }
    }

    /// Obsidian-style: stable IDs minted lazily (link-by-id or publish), and
    /// prov authors structural links *by* id — so a move rewrites nothing,
    /// the registry keeps them resolving. Portable path links for the rest.
    pub fn stable_ids() -> Self {
        Self {
            identity: Registration::LAZY,
            reference_target: Addressing::Id,
            id_storage: IdStorage::Registry,
            ..Self::default()
        }
    }

    /// The fused path [`LinkStyle`] this config's notation + path resolution
    /// select — what the [`Workspace`](crate::workspace::Workspace) builder's
    /// `link_style` expects for authoring structural path links.
    pub fn link_format(&self) -> LinkStyle {
        LinkStyle::from_axes(self.notation, self.path_style)
    }

    /// The effective workspace-default [`ReferenceStyle`] — the fallback for any
    /// relation without its own override, composed from the four reference axes.
    pub fn reference_style(&self) -> ReferenceStyle {
        ReferenceStyle {
            wrapper: self.notation.wrapper(),
            addressing: self.reference_target,
            label: self.reference_label,
            path_style: LinkStyle::from_axes(self.notation, self.path_style),
        }
        .normalized()
    }

    /// The declared per-relation overrides resolved to full [`ReferenceStyle`]s,
    /// each partial overlaid on the workspace default ([`reference_style`]) and
    /// normalized. Feed the result to
    /// [`RelationSet::with_styles`](prov_graph::relation::RelationSet::with_styles) to
    /// build the workspace's relation vocabulary from a config. Empty when no
    /// relation declares an override — every relation then inherits the default.
    ///
    /// [`reference_style`]: Self::reference_style
    pub fn resolved_relation_styles(&self) -> BTreeMap<String, ReferenceStyle> {
        let base = self.reference_style();
        let base_notation = Notation::from_wrapper(base.wrapper, base.path_style);
        let base_path = base.path_style.axes().1;
        self.relation_styles
            .iter()
            .map(|(name, over)| {
                let notation = over.notation.unwrap_or(base_notation);
                let path = over.path_style.unwrap_or(base_path);
                let style = ReferenceStyle {
                    wrapper: notation.wrapper(),
                    addressing: over.target.unwrap_or(base.addressing),
                    label: over.label.unwrap_or(base.label),
                    path_style: LinkStyle::from_axes(notation, path),
                }
                .normalized();
                (name.clone(), style)
            })
            .collect()
    }

    /// Build this workspace's relation vocabulary — the self-describing path
    /// (DESIGN §1, the `prov/1` spec). When [`relation_defs`](Self::relation_defs)
    /// is **empty**, this is the diaryx preset
    /// ([`RelationSet::diaryx`](prov_graph::relation::RelationSet::diaryx)) unchanged —
    /// graceful degradation, so a minimal vault that spells out nothing keeps
    /// working. When it declares definitions, the vocabulary is built from them,
    /// and the structural pointer relations (`registry`/`config`/`recycle_bin`)
    /// are preserved so those pointers stay reachable regardless. An explicit
    /// `spanning` always wins; per-relation reference styles are overlaid last.
    pub fn relation_set(&self) -> RelationSet {
        let mut set = if self.relation_defs.is_empty() {
            RelationSet::diaryx()
        } else {
            let mut s = RelationSet::new();
            for (name, def) in &self.relation_defs {
                let mut rel = match def.cardinality.unwrap_or(Cardinality::Many) {
                    Cardinality::One => Relation::one(name),
                    Cardinality::Many => Relation::many(name),
                };
                if let Some(inverse) = &def.inverse {
                    rel = rel.inverse(inverse);
                }
                s = s.with(rel);
            }
            // Keep the structural pointer relations reachable even under a fully
            // custom vocabulary — but never shadow one the user already declared.
            for pointer in ["registry", "config", "recycle_bin", "history", "about"] {
                if !s.relations().iter().any(|r| r.name == pointer) {
                    s = s.with(Relation::one(pointer));
                }
            }
            s.registry("registry")
                .config("config")
                .recycle("recycle_bin")
                .history("history")
                .about("about")
        };
        if let Some(spanning) = &self.spanning {
            set = set.spanning(spanning);
        }
        set.with_styles(&self.resolved_relation_styles())
    }

    /// Whether a *mutation* under this config could mint a new stable ID — so a
    /// caller that will land one must bootstrap a registry document *first*
    /// (before the change set that would otherwise strand the id→path map with no
    /// home). Two ways an op mints: an **eager** identity policy stamps every
    /// created document, and any **id-registering reference style** (the workspace
    /// default, or a single relation's override — e.g. `part_of: id` in a split)
    /// registers a link's target when a `link` fires.
    ///
    /// This is the single home for a judgment the CLI previously recomputed at
    /// every mutation command (`new`, `attach`, `mv --in`, `reparent`,
    /// `duplicate`, `init`'s adoption pass), each an identical copy of the same
    /// three-line `link_registers && fires_on(Link) || fires_on(Create)` — the
    /// kind of duplicated policy that drifts silently. It lives here because every
    /// term it needs is a fact about the config.
    pub fn mints_on_mutation(&self) -> bool {
        let link_registers = self.reference_style().registers()
            || self
                .resolved_relation_styles()
                .values()
                .any(|s| s.registers());
        (link_registers && self.identity.fires_on(Trigger::Link))
            || self.identity.fires_on(Trigger::Create)
    }

    /// Overlay the recognized keys present in `meta` onto this config; absent
    /// keys keep their current value. `meta` is either a root's `prov:` block
    /// or a config document's top-level mapping — the same nested shape. Apply the
    /// root block first, then the config document, so the config document wins.
    pub fn apply(&mut self, meta: &Value) {
        if let Some(v) = meta
            .get("content_format")
            .and_then(Value::as_str)
            .and_then(ContentFormat::from_config_str)
        {
            self.content_format = v;
        }
        if let Some(md) = meta.get("metadata") {
            if let Some(v) = md
                .get("format")
                .and_then(Value::as_str)
                .and_then(format_from_str)
            {
                self.default_embed_format = v;
            }
            if let Some(v) = md
                .get("embed")
                .and_then(Value::as_str)
                .and_then(EmbedStyle::from_config_str)
            {
                self.embed_style = v;
            }
        }
        if let Some(rf) = meta.get("references") {
            if let Some(v) = rf
                .get("notation")
                .and_then(Value::as_str)
                .and_then(Notation::from_config_str)
            {
                self.notation = v;
            }
            if let Some(v) = rf
                .get("path_style")
                .and_then(Value::as_str)
                .and_then(PathStyle::from_config_str)
            {
                self.path_style = v;
            }
            if let Some(v) = rf
                .get("target")
                .and_then(Value::as_str)
                .and_then(Addressing::from_config_str)
            {
                self.reference_target = v;
            }
            if let Some(v) = rf.get("label").and_then(Value::as_bool) {
                self.reference_label = v;
            }
        }
        // The spanning relation (self-description, §3): a top-level field name.
        if let Some(v) = meta.get("spanning").and_then(Value::as_str) {
            self.spanning = Some(v.to_string());
        }
        // What the workspace calls itself. A malformed name is ignored here and
        // reported by `diagnose` — honoring half of it would mean a reference
        // that round-trips through a name prov cannot actually write.
        if let Some(v) = meta
            .get("workspace_id")
            .and_then(Value::as_str)
            .filter(|v| is_valid_workspace_id(v))
        {
            self.workspace_id = v.to_string();
        }
        // Per-relation entries carry two orthogonal halves in one block:
        // *style* overrides (`notation`/`path_style`/`target`/`label`) and
        // structural *definitions* (`cardinality`/`inverse`/`means`).
        if let Some(relations) = meta.get("relations").and_then(Value::as_mapping) {
            for (name, spec) in relations {
                let entry = self.relation_styles.entry(name.clone()).or_default();
                if let Some(v) = spec
                    .get("notation")
                    .and_then(Value::as_str)
                    .and_then(Notation::from_config_str)
                {
                    entry.notation = Some(v);
                }
                if let Some(v) = spec
                    .get("path_style")
                    .and_then(Value::as_str)
                    .and_then(PathStyle::from_config_str)
                {
                    entry.path_style = Some(v);
                }
                if let Some(v) = spec
                    .get("target")
                    .and_then(Value::as_str)
                    .and_then(Addressing::from_config_str)
                {
                    entry.target = Some(v);
                }
                if let Some(v) = spec.get("label").and_then(Value::as_bool) {
                    entry.label = Some(v);
                }
                // The structural half — only recorded when at least one def key is
                // present, so a style-only entry does not synthesize an empty def.
                let cardinality = spec
                    .get("cardinality")
                    .and_then(Value::as_str)
                    .and_then(cardinality_from_str);
                let inverse = spec
                    .get("inverse")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let means = spec
                    .get("means")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if cardinality.is_some() || inverse.is_some() || means.is_some() {
                    let def = self.relation_defs.entry(name.clone()).or_default();
                    if cardinality.is_some() {
                        def.cardinality = cardinality;
                    }
                    if inverse.is_some() {
                        def.inverse = inverse;
                    }
                    if means.is_some() {
                        def.means = means;
                    }
                }
            }
        }
        // Field declarations: `fields: { <field>: { type, values, vocabulary, reify } }`.
        if let Some(fields) = meta.get("fields").and_then(Value::as_mapping) {
            for (name, spec) in fields {
                let vocabulary = spec
                    .get("vocabulary")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let ty = spec
                    .get("type")
                    .and_then(Value::as_str)
                    .and_then(field_type_from_config_str);
                // An entry that declares neither a type nor a vocabulary says
                // nothing about the field that prov or a frontend could act on;
                // recording it would only claim the field is described when it
                // isn't. (`diagnose` reports the malformed spelling that most
                // often causes this.)
                if ty.is_none() && vocabulary.is_none() {
                    continue;
                }
                let values = spec
                    .get("values")
                    .and_then(Value::as_str)
                    .and_then(OpenClosed::from_config_str)
                    .unwrap_or_default();
                let reify = spec.get("reify").and_then(Value::as_bool).unwrap_or(false);
                self.fields.insert(
                    name.clone(),
                    FieldSpec {
                        ty,
                        values,
                        vocabulary,
                        reify,
                    },
                );
            }
        }
        // View declarations: `views: { <name>: { group, by, under, nest, … } }`.
        //
        // Merged per entry, exactly as `fields` is and for the same reason: a
        // vault config that declares one view must not wipe the ones the app's
        // defaults supplied. A later surface redeclaring a name replaces that
        // view whole — a view is small and its keys interlock (`by` means
        // nothing without `group`), so merging *within* one would produce
        // hybrids no surface wrote.
        if let Some(views) = meta.get(prov_views::VIEWS_KEY).and_then(Value::as_mapping) {
            for (name, value) in views {
                let Some(spec) = ViewSpec::parse(name, value) else {
                    continue;
                };
                match self.views.iter_mut().find(|v| v.name == spec.name) {
                    Some(existing) => *existing = spec,
                    None => self.views.push(spec),
                }
            }
        }
        // Export declarations: `exports: { <name>: { gate, view, … } }`.
        // Merged per entry like `views` — and replacement is whole for a
        // sharper reason than key interlock: an export half-merged across two
        // surfaces would bound what leaves with a gate neither surface wrote.
        // An entry `parse` cannot make a gate of is dropped (fail closed — it
        // exports nothing) and `diagnose` is where the reason surfaces.
        if let Some(exports) = meta
            .get(prov_exports::EXPORTS_KEY)
            .and_then(Value::as_mapping)
        {
            for (name, value) in exports {
                let Some(spec) = ExportSpec::parse(name, value) else {
                    continue;
                };
                match self.exports.iter_mut().find(|e| e.name == spec.name) {
                    Some(existing) => *existing = spec,
                    None => self.exports.push(spec),
                }
            }
        }
        if let Some(v) = meta
            .get("id_storage")
            .and_then(Value::as_str)
            .and_then(IdStorage::from_config_str)
        {
            self.id_storage = v;
        }
        if let Some(v) = meta.get("updated").and_then(Value::as_str) {
            self.updated = v.to_string();
        }
        if let Some(v) = meta
            .get("identity")
            .and_then(Value::as_str)
            .and_then(registration_from_str)
        {
            self.identity = v;
        }
        if let Some(v) = meta
            .get("fixity")
            .and_then(Value::as_str)
            .and_then(Fixity::from_config_str)
        {
            self.fixity = v;
        }
        if let Some(v) = meta.get("recycle_bin").and_then(Value::as_bool) {
            self.recycle_bin = v;
        }
        if let Some(v) = meta
            .get("history")
            .and_then(Value::as_str)
            .and_then(History::from_config_str)
        {
            self.history = v;
        }
        if let Some(v) = meta
            .get("about")
            .and_then(Value::as_str)
            .and_then(About::from_config_str)
        {
            self.about = v;
        }
    }

    /// A fresh config with `meta`'s recognized keys applied over the defaults.
    pub fn from_meta(meta: &Value) -> Self {
        let mut config = Self::default();
        config.apply(meta);
        config
    }

    /// This config as config-document metadata keys (the nested vocabulary,
    /// `docs/config-vocab.md`). Emitted at the top level of the config document;
    /// the same mapping nests under `prov:` in a root's frontmatter.
    pub fn to_mapping(&self) -> Mapping {
        let mut map = Mapping::new();
        map.insert("spec".into(), Value::Int(SPEC_VERSION));
        map.insert(
            "content_format".into(),
            Value::String(self.content_format.as_config_str().into()),
        );

        let mut metadata = Mapping::new();
        metadata.insert(
            "format".into(),
            Value::String(format_str(self.default_embed_format).into()),
        );
        metadata.insert(
            "embed".into(),
            Value::String(self.embed_style.as_config_str().into()),
        );
        map.insert("metadata".into(), Value::Mapping(metadata));

        let mut references = Mapping::new();
        references.insert(
            "notation".into(),
            Value::String(self.notation.as_config_str().into()),
        );
        references.insert(
            "path_style".into(),
            Value::String(self.path_style.as_config_str().into()),
        );
        references.insert(
            "target".into(),
            Value::String(self.reference_target.as_config_str().into()),
        );
        references.insert("label".into(), Value::Bool(self.reference_label));
        map.insert("references".into(), Value::Mapping(references));

        if let Some(spanning) = &self.spanning {
            map.insert("spanning".into(), Value::String(spanning.clone()));
        }

        // One `relations` block carries both halves of each entry — style
        // overrides and structural definitions — so the union of the two maps'
        // keys is emitted, each entry merging whichever halves it has.
        if !self.relation_styles.is_empty() || !self.relation_defs.is_empty() {
            let mut names: Vec<&String> = self
                .relation_styles
                .keys()
                .chain(self.relation_defs.keys())
                .collect();
            names.sort();
            names.dedup();
            let mut relations = Mapping::new();
            for name in names {
                let mut spec = Mapping::new();
                if let Some(over) = self.relation_styles.get(name) {
                    if let Some(n) = over.notation {
                        spec.insert("notation".into(), Value::String(n.as_config_str().into()));
                    }
                    if let Some(p) = over.path_style {
                        spec.insert("path_style".into(), Value::String(p.as_config_str().into()));
                    }
                    if let Some(t) = over.target {
                        spec.insert("target".into(), Value::String(t.as_config_str().into()));
                    }
                    if let Some(l) = over.label {
                        spec.insert("label".into(), Value::Bool(l));
                    }
                }
                if let Some(def) = self.relation_defs.get(name) {
                    if let Some(c) = def.cardinality {
                        spec.insert(
                            "cardinality".into(),
                            Value::String(cardinality_str(c).into()),
                        );
                    }
                    if let Some(inv) = &def.inverse {
                        spec.insert("inverse".into(), Value::String(inv.clone()));
                    }
                    if let Some(m) = &def.means {
                        spec.insert("means".into(), Value::String(m.clone()));
                    }
                }
                relations.insert(name.clone(), Value::Mapping(spec));
            }
            map.insert("relations".into(), Value::Mapping(relations));
        }

        if !self.fields.is_empty() {
            let mut fields = Mapping::new();
            for (name, spec) in &self.fields {
                let mut entry = Mapping::new();
                if let Some(ty) = spec.ty.and_then(field_type_as_config_str) {
                    entry.insert("type".into(), Value::String(ty.into()));
                }
                // `values` describes a vocabulary, so it is only meaningful — and
                // only written — alongside one.
                if let Some(vocabulary) = &spec.vocabulary {
                    entry.insert(
                        "values".into(),
                        Value::String(spec.values.as_config_str().into()),
                    );
                    entry.insert("vocabulary".into(), Value::String(vocabulary.clone()));
                }
                if spec.reify {
                    entry.insert("reify".into(), Value::Bool(true));
                }
                fields.insert(name.clone(), Value::Mapping(entry));
            }
            map.insert("fields".into(), Value::Mapping(fields));
        }

        if !self.views.is_empty() {
            let mut views = Mapping::new();
            for spec in &self.views {
                views.insert(spec.name.clone(), Value::Mapping(spec.to_mapping()));
            }
            map.insert(prov_views::VIEWS_KEY.into(), Value::Mapping(views));
        }

        if !self.exports.is_empty() {
            let mut exports = Mapping::new();
            for spec in &self.exports {
                exports.insert(spec.name.clone(), Value::Mapping(spec.to_mapping()));
            }
            map.insert(prov_exports::EXPORTS_KEY.into(), Value::Mapping(exports));
        }

        map.insert(
            "id_storage".into(),
            Value::String(self.id_storage.as_config_str().into()),
        );
        map.insert("updated".into(), Value::String(self.updated.clone()));
        map.insert(
            "identity".into(),
            Value::String(registration_str(self.identity).into()),
        );
        map.insert(
            "fixity".into(),
            Value::String(self.fixity.as_config_str().into()),
        );
        map.insert("recycle_bin".into(), Value::Bool(self.recycle_bin));
        map.insert(
            "history".into(),
            Value::String(self.history.as_config_str().into()),
        );
        map.insert(
            "about".into(),
            Value::String(self.about.as_config_str().into()),
        );
        map.insert(
            "workspace_id".into(),
            Value::String(self.workspace_id.clone()),
        );
        map
    }
}

// ── Config linting (`docs/config-vocab.md`, "Linting") ──────────────────────

/// A key in a config surface that [`WorkspaceConfig::apply`] would silently
/// ignore — surfaced so a setting that never takes effect becomes visible rather
/// than staying invisible. `apply` keeps the current value whenever a key is
/// unrecognized or its value fails to parse; that robustness is what makes a
/// typo (`notaton`) or a bad value (`fixity: alll`) vanish without a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    /// The offending key, dotted from the block root (`references.notation`).
    pub key: String,
    /// What is wrong with it.
    pub kind: ConfigIssueKind,
}

/// The two ways a config key goes unread. See [`ConfigIssue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigIssueKind {
    /// `key` is not a recognized axis but closely resembles `suggestion` — almost
    /// certainly a misspelling. An unrecognized key that resembles *no* axis at
    /// its level is deliberately **not** reported: a config surface can carry
    /// user-owned fields prov never reads (DESIGN §2), so flagging every
    /// unknown key would be noise.
    UnknownKey { suggestion: String },
    /// `key` is a recognized axis but `value` is not a spelling prov
    /// understands, so `apply` kept the default. `expected` lists the accepted
    /// spellings (advisory help; mirrors the axis's parser).
    InvalidValue {
        value: String,
        expected: Vec<String>,
    },
    /// The `spanning` relation's declared `inverse` is a relation whose
    /// cardinality is `many`, which cannot form the single-parent containment
    /// tree the spanning relation requires (DESIGN §3). `key` is `spanning`;
    /// `inverse` is the offending child→parent relation.
    SpanningNotSingleParent { inverse: String },
    /// A view declares `nest:` but groups by a field the workspace declares
    /// multi-valued (`fields.<field>.type: seq`).
    ///
    /// Nesting files a record into the single-parent spanning relation, so a
    /// document carrying two values for `field` has two homes and nothing can
    /// choose between them. The *grouping* is fine — one document under several
    /// groups is what a view is for — so only the filing half is reported.
    NestNotSingleValued { field: String },
    /// `workspace_id` holds a name that cannot be written as the qualifier of an
    /// `id:<workspace>/<id>` reference — it contains `/`, `:` or whitespace, or
    /// is not a string at all. `apply` ignored it, so the workspace stayed
    /// anonymous.
    ///
    /// An **empty** value is not this: it is the explicit spelling of anonymous,
    /// the way an empty `updated` spells that feature off.
    ///
    /// Unlike [`InvalidValue`](Self::InvalidValue) there is no list of accepted
    /// spellings to offer: the name is the user's to choose and only its *shape*
    /// is constrained.
    MalformedWorkspaceId { value: String },
}

/// Top-level config keys (block names + scalar axes + the `spec` marker).
const TOP_KEYS: &[&str] = &[
    "spec",
    "content_format",
    "metadata",
    "references",
    "relations",
    "spanning",
    "fields",
    "views",
    "exports",
    "id_storage",
    "updated",
    "workspace_id",
    "identity",
    "fixity",
    "recycle_bin",
    "history",
    "about",
];
/// Keys inside the `metadata:` block.
const METADATA_KEYS: &[&str] = &["format", "embed"];
/// The reference-style keys valid in the `references:` block and in each
/// `relations.<name>` entry.
const REFERENCE_KEYS: &[&str] = &["notation", "path_style", "target", "label"];
/// The structural definition keys valid only in a `relations.<name>` entry
/// (`means` is free-form and never near-miss-matched, like `updated`).
const RELATION_DEF_KEYS: &[&str] = &["cardinality", "inverse", "means"];
/// Keys inside each `fields.<name>` entry.
const FIELD_KEYS: &[&str] = &["type", "values", "vocabulary", "reify"];

/// If `meta` declares a `spec` newer than [`SPEC_VERSION`] — the version this
/// build understands — the declared version. The signal that prov may be
/// silently ignoring settings a newer prov wrote. `None` when `spec` is
/// absent, not an integer, or within range. Shared by `check` (a
/// `Finding::ConfigSpecAhead`) and the CLI's proactive config warning, so the
/// version comparison lives in one place.
pub fn spec_ahead(meta: &Value) -> Option<i64> {
    match meta.get("spec") {
        Some(Value::Int(v)) if *v > SPEC_VERSION => Some(*v),
        _ => None,
    }
}

/// Diagnose a config surface (a root's `prov:` block or a config document's
/// top-level mapping): one [`ConfigIssue`] per key `apply` would silently ignore.
/// Recognized keys are checked for a value prov can parse; unrecognized keys
/// are reported only when they closely resemble a real axis at their level (a
/// likely typo). Returns empty for a clean config.
pub fn diagnose(meta: &Value) -> Vec<ConfigIssue> {
    let mut issues = Vec::new();
    let Some(map) = meta.as_mapping() else {
        return issues;
    };
    for (key, value) in map {
        match key.as_str() {
            "spec" => {} // version marker — not a policy axis
            "content_format" => {
                enum_axis(
                    &mut issues,
                    key,
                    value,
                    |s| ContentFormat::from_config_str(s).is_some(),
                    &["markdown", "djot", "html"],
                );
            }
            "id_storage" => {
                enum_axis(
                    &mut issues,
                    key,
                    value,
                    |s| IdStorage::from_config_str(s).is_some(),
                    &["registry", "frontmatter", "both"],
                );
            }
            "identity" => {
                enum_axis(
                    &mut issues,
                    key,
                    value,
                    |s| registration_from_str(s).is_some(),
                    &["none", "lazy", "eager"],
                );
            }
            "fixity" => {
                enum_axis(
                    &mut issues,
                    key,
                    value,
                    |s| Fixity::from_config_str(s).is_some(),
                    &["off", "attachments", "all"],
                );
            }
            "recycle_bin" => bool_axis(&mut issues, key, value),
            "history" => {
                enum_axis(
                    &mut issues,
                    key,
                    value,
                    |s| History::from_config_str(s).is_some(),
                    &["off", "manual"],
                );
            }
            "about" => {
                enum_axis(
                    &mut issues,
                    key,
                    value,
                    |s| About::from_config_str(s).is_some(),
                    &["off", "structure"],
                );
            }
            "updated" => {} // free-form field name
            // A name the user chose, constrained only in shape — it has to
            // survive being written as the qualifier of an `id:<ws>/<id>`
            // target. A non-string is malformed for the same reason.
            //
            // The empty string is *not*: it is the explicit spelling of the
            // default (anonymous), exactly as an empty `updated` spells the
            // stamping feature off. `to_mapping` writes it that way, so
            // flagging it would make prov's own serialized default fail its own
            // diagnosis.
            "workspace_id" => {
                let ok = match value.as_str() {
                    Some(s) => s.is_empty() || is_valid_workspace_id(s),
                    None => false,
                };
                if !ok {
                    issues.push(ConfigIssue {
                        key: key.clone(),
                        kind: ConfigIssueKind::MalformedWorkspaceId {
                            value: value_summary(value),
                        },
                    });
                }
            }
            "spanning" => {
                // A relation name — must be a string; its coherence with the
                // relations block is a cross-relation check below.
                if value.as_str().is_none() {
                    issues.push(ConfigIssue {
                        key: key.clone(),
                        kind: ConfigIssueKind::InvalidValue {
                            value: value_summary(value),
                            expected: vec!["a relation name".into()],
                        },
                    });
                }
            }
            "metadata" => diagnose_metadata(&mut issues, value),
            "references" => diagnose_reference_block(&mut issues, "references", value),
            "relations" => diagnose_relations(&mut issues, value),
            "fields" => diagnose_fields(&mut issues, value),
            "views" => diagnose_views(&mut issues, value, map),
            "exports" => diagnose_exports(&mut issues, value, map),
            other => {
                if let Some(suggestion) = nearest(other, TOP_KEYS) {
                    issues.push(unknown(key.clone(), suggestion));
                }
            }
        }
    }
    diagnose_spanning_invariant(&mut issues, map);
    issues
}

/// The single-parent invariant (DESIGN §3): if `spanning` names a declared
/// relation whose declared `inverse` is itself declared with `cardinality: many`,
/// that inverse cannot be the child→parent side of a tree — reported so an
/// incoherent vocabulary is caught at author time rather than surfacing as a
/// runtime `DuplicateContainment` finding. Absence (an undeclared inverse, or a
/// spanning relation built into the vocabulary rather than declared) is left
/// alone — only a *declared contradiction* is flagged, never under-specification.
fn diagnose_spanning_invariant(issues: &mut Vec<ConfigIssue>, map: &Mapping) {
    let Some(spanning) = map.get("spanning").and_then(Value::as_str) else {
        return;
    };
    let Some(relations) = map.get("relations").and_then(Value::as_mapping) else {
        return;
    };
    let Some(inverse) = relations
        .get(spanning)
        .and_then(Value::as_mapping)
        .and_then(|r| r.get("inverse"))
        .and_then(Value::as_str)
    else {
        return;
    };
    let inverse_cardinality = relations
        .get(inverse)
        .and_then(Value::as_mapping)
        .and_then(|r| r.get("cardinality"))
        .and_then(Value::as_str);
    if inverse_cardinality == Some("many") {
        issues.push(ConfigIssue {
            key: "spanning".into(),
            kind: ConfigIssueKind::SpanningNotSingleParent {
                inverse: inverse.to_string(),
            },
        });
    }
}

/// Diagnose the `metadata:` block.
fn diagnose_metadata(issues: &mut Vec<ConfigIssue>, value: &Value) {
    let Some(map) = value.as_mapping() else {
        return block_shape_issue(issues, "metadata", value);
    };
    for (key, v) in map {
        let dotted = format!("metadata.{key}");
        match key.as_str() {
            "format" => enum_axis(
                issues,
                &dotted,
                v,
                |s| format_from_str(s).is_some(),
                &embed_format_spellings(),
            ),
            "embed" => enum_axis(
                issues,
                &dotted,
                v,
                |s| EmbedStyle::from_config_str(s).is_some(),
                &[
                    "delimited",
                    "code_block",
                    "html_script",
                    "html_code",
                    "separate",
                ],
            ),
            other => {
                if let Some(sug) = nearest(other, METADATA_KEYS) {
                    issues.push(unknown(dotted, format!("metadata.{sug}")));
                }
            }
        }
    }
}

/// Diagnose a `references:`-shaped block (the workspace default or a
/// `relations.<name>` entry), `prefix` dotting the reported keys.
fn diagnose_reference_block(issues: &mut Vec<ConfigIssue>, prefix: &str, value: &Value) {
    let Some(map) = value.as_mapping() else {
        return block_shape_issue(issues, prefix, value);
    };
    for (key, v) in map {
        let dotted = format!("{prefix}.{key}");
        match key.as_str() {
            "notation" => enum_axis(
                issues,
                &dotted,
                v,
                |s| Notation::from_config_str(s).is_some(),
                &["markdown", "wikilink", "bare"],
            ),
            "path_style" => enum_axis(
                issues,
                &dotted,
                v,
                |s| PathStyle::from_config_str(s).is_some(),
                &["root", "relative"],
            ),
            "target" => enum_axis(
                issues,
                &dotted,
                v,
                |s| Addressing::from_config_str(s).is_some(),
                &["path", "id", "alias"],
            ),
            "label" => bool_axis(issues, &dotted, v),
            other => {
                if let Some(sug) = nearest(other, REFERENCE_KEYS) {
                    issues.push(unknown(dotted, format!("{prefix}.{sug}")));
                }
            }
        }
    }
}

/// Diagnose the `relations:` block — a mapping of relation name to an entry that
/// may carry both reference-style keys and structural definition keys.
fn diagnose_relations(issues: &mut Vec<ConfigIssue>, value: &Value) {
    let Some(map) = value.as_mapping() else {
        return block_shape_issue(issues, "relations", value);
    };
    for (name, spec) in map {
        diagnose_relation_entry(issues, name, spec);
    }
}

/// Diagnose one `relations.<name>` entry: the reference-style axes
/// ([`REFERENCE_KEYS`]) plus the structural definition keys
/// ([`RELATION_DEF_KEYS`]). `means` is free-form and accepted without check;
/// `cardinality` is enum-checked; `inverse` must be a string. An unknown key is
/// reported only when it near-misses a valid key at this level.
fn diagnose_relation_entry(issues: &mut Vec<ConfigIssue>, name: &str, value: &Value) {
    let prefix = format!("relations.{name}");
    let Some(map) = value.as_mapping() else {
        return block_shape_issue(issues, &prefix, value);
    };
    for (key, v) in map {
        let dotted = format!("{prefix}.{key}");
        match key.as_str() {
            "notation" => enum_axis(
                issues,
                &dotted,
                v,
                |s| Notation::from_config_str(s).is_some(),
                &["markdown", "wikilink", "bare"],
            ),
            "path_style" => enum_axis(
                issues,
                &dotted,
                v,
                |s| PathStyle::from_config_str(s).is_some(),
                &["root", "relative"],
            ),
            "target" => enum_axis(
                issues,
                &dotted,
                v,
                |s| Addressing::from_config_str(s).is_some(),
                &["path", "id", "alias"],
            ),
            "label" => bool_axis(issues, &dotted, v),
            "cardinality" => enum_axis(
                issues,
                &dotted,
                v,
                |s| cardinality_from_str(s).is_some(),
                &["one", "many"],
            ),
            "inverse" => {
                if v.as_str().is_none() {
                    issues.push(ConfigIssue {
                        key: dotted,
                        kind: ConfigIssueKind::InvalidValue {
                            value: value_summary(v),
                            expected: vec!["a relation name".into()],
                        },
                    });
                }
            }
            "means" => {} // free-form human gloss — carried, not read (§2)
            other => {
                let mut valid: Vec<&str> = REFERENCE_KEYS.to_vec();
                valid.extend_from_slice(RELATION_DEF_KEYS);
                if let Some(sug) = nearest(other, &valid) {
                    issues.push(unknown(dotted, format!("{prefix}.{sug}")));
                }
            }
        }
    }
}

/// Diagnose the `fields:` block — a mapping of frontmatter field name to a field
/// declaration (`type` / `values` / `vocabulary` / `reify`).
fn diagnose_fields(issues: &mut Vec<ConfigIssue>, value: &Value) {
    let Some(map) = value.as_mapping() else {
        return block_shape_issue(issues, "fields", value);
    };
    for (name, spec) in map {
        let prefix = format!("fields.{name}");
        let Some(entry) = spec.as_mapping() else {
            block_shape_issue(issues, &prefix, spec);
            continue;
        };
        for (key, v) in entry {
            let dotted = format!("{prefix}.{key}");
            match key.as_str() {
                "type" => enum_axis(
                    issues,
                    &dotted,
                    v,
                    |s| field_type_from_config_str(s).is_some(),
                    FIELD_TYPES,
                ),
                "values" => enum_axis(
                    issues,
                    &dotted,
                    v,
                    |s| OpenClosed::from_config_str(s).is_some(),
                    &["open", "closed"],
                ),
                "vocabulary" => {
                    if v.as_str().is_none() {
                        issues.push(ConfigIssue {
                            key: dotted,
                            kind: ConfigIssueKind::InvalidValue {
                                value: value_summary(v),
                                expected: vec!["a link to a vocabulary document".into()],
                            },
                        });
                    }
                }
                "reify" => bool_axis(issues, &dotted, v),
                other => {
                    if let Some(sug) = nearest(other, FIELD_KEYS) {
                        issues.push(unknown(dotted, format!("{prefix}.{sug}")));
                    }
                }
            }
        }
    }
}

/// Diagnose the `views:` block — a mapping of view name to a view declaration.
///
/// The judgment is `prov-views`' (one definition of what a view is, shared with
/// the crate that executes one); this is the translation into config-issue
/// vocabulary, plus the near-miss suggestion, which needs the edit distance
/// every other config near-miss already uses.
fn diagnose_views(issues: &mut Vec<ConfigIssue>, value: &Value, surface: &Mapping) {
    let Some(map) = value.as_mapping() else {
        return block_shape_issue(issues, "views", value);
    };
    for (name, spec) in map {
        let prefix = format!("views.{name}");
        diagnose_nest_is_fileable(issues, &prefix, spec, surface);
        for issue in prov_views::diagnose_view(name, spec) {
            let dotted = match issue.key.as_str() {
                "" => prefix.clone(),
                key => format!("{prefix}.{key}"),
            };
            let expected = || issue.kind.expected().iter().map(|s| (*s).into()).collect();
            match &issue.kind {
                ViewIssueKind::NotAMapping => block_shape_issue(issues, &prefix, spec),
                ViewIssueKind::NoGrouping => issues.push(ConfigIssue {
                    key: dotted,
                    kind: ConfigIssueKind::InvalidValue {
                        value: spec
                            .get("group")
                            .map_or_else(|| "(absent)".to_string(), value_summary),
                        expected: vec![
                            "a field name, or a list of field names to try in order".into(),
                        ],
                    },
                }),
                ViewIssueKind::BadGrain => issues.push(ConfigIssue {
                    key: dotted.clone(),
                    kind: ConfigIssueKind::InvalidValue {
                        value: spec
                            .get(&issue.key)
                            .map_or_else(|| "(absent)".to_string(), value_summary),
                        expected: expected(),
                    },
                }),
                ViewIssueKind::NoCondition => issues.push(ConfigIssue {
                    key: dotted,
                    kind: ConfigIssueKind::InvalidValue {
                        value: spec
                            .get("where")
                            .map_or_else(|| "(absent)".to_string(), value_summary),
                        expected: expected(),
                    },
                }),
                // Unlike a stray *top-level* key — which may be a user-owned
                // field prov never reads (DESIGN §2) — a stray key inside a
                // `views.<name>` entry is inside a block prov defines
                // completely, so a near-miss is the only thing it can be.
                ViewIssueKind::UnknownKey => {
                    if let Some(sug) = nearest(&issue.key, prov_views::VIEW_KEYS) {
                        issues.push(unknown(dotted, format!("{prefix}.{sug}")));
                    }
                }
            }
        }
    }
}

/// Flag a `nest:` on a view that groups by a field the workspace declares
/// **multi-valued** (`fields.<name>.type: seq`).
///
/// `nest` files a record into the spanning relation, which is single-parent, so
/// a document with two values for the grouping field has two homes and no way
/// to choose between them. Grouping by such a field is perfectly good — that is
/// the whole point of a view — so this flags only the *filing* half.
///
/// Reported rather than left to bite later because `nest:` is a description a
/// frontend acts on, so the failure surfaces at the moment someone creates a
/// document, which is the worst time to discover it. `ViewSpec::nest_route`
/// returns `None` for the same case at runtime, so the two agree.
///
/// Only fires when `fields` and `views` are declared in the **same config
/// surface**: `diagnose` lints one surface at a time and cannot see the merged
/// config, which is the same bound every other cross-key check here has.
fn diagnose_nest_is_fileable(
    issues: &mut Vec<ConfigIssue>,
    prefix: &str,
    spec: &Value,
    surface: &Mapping,
) {
    if spec.get("nest").is_none() {
        return;
    }
    let Some(fields) = surface.get("fields").and_then(Value::as_mapping) else {
        return;
    };
    let Some(view) = prov_views::ViewSpec::parse("", spec) else {
        return;
    };
    // Any key in the chain being multi-valued is enough: the chain picks
    // whichever is filled in, so a document could reach the `seq` one.
    let multi: Vec<&String> = view
        .group
        .keys
        .iter()
        .filter(|key| {
            fields
                .get(*key)
                .and_then(|f| f.get("type"))
                .and_then(Value::as_str)
                .and_then(field_type_from_config_str)
                == Some(FieldType::Seq)
        })
        .collect();
    if let Some(field) = multi.first() {
        issues.push(ConfigIssue {
            key: format!("{prefix}.nest"),
            kind: ConfigIssueKind::NestNotSingleValued {
                field: (*field).clone(),
            },
        });
    }
}

/// Diagnose the `exports:` block — a mapping of export name to an export
/// declaration.
///
/// The judgment is `prov-exports`' (one definition of what an export is,
/// shared with the crate that plans one); this is the translation into
/// config-issue vocabulary, plus the near-miss suggestion. The stakes of the
/// translation are asymmetric here: a dropped export publishes *nothing*, so
/// every fatal issue below is a declaration someone wrote that silently does
/// not exist until this report says so.
fn diagnose_exports(issues: &mut Vec<ConfigIssue>, value: &Value, surface: &Mapping) {
    let Some(map) = value.as_mapping() else {
        return block_shape_issue(issues, "exports", value);
    };
    for (name, spec) in map {
        let prefix = format!("exports.{name}");
        diagnose_export_view_is_declared(issues, &prefix, spec, surface);
        for issue in prov_exports::diagnose_export(name, spec) {
            match &issue.kind {
                ExportIssueKind::NotAMapping => block_shape_issue(issues, &prefix, spec),
                ExportIssueKind::NoGate => issues.push(ConfigIssue {
                    key: format!("{prefix}.gate"),
                    kind: ConfigIssueKind::InvalidValue {
                        value: spec
                            .get("gate")
                            .map_or_else(|| "(absent)".to_string(), value_summary),
                        expected: vec![
                            "a mapping with `field` and `value` — the field a document \
                             declares its membership in, and the value that admits it"
                                .into(),
                        ],
                    },
                }),
                // A stray key inside an `exports.<name>` entry (or its gate)
                // is inside a block prov defines completely, so a near-miss is
                // the only thing it can be — same reasoning as `views`.
                ExportIssueKind::UnknownKey => {
                    if let Some(sug) = nearest(&issue.key, prov_exports::EXPORT_KEYS) {
                        issues.push(unknown(
                            format!("{prefix}.{}", issue.key),
                            format!("{prefix}.{sug}"),
                        ));
                    }
                }
                ExportIssueKind::GateUnknownKey => {
                    if let Some(sug) = nearest(&issue.key, prov_exports::GATE_KEYS) {
                        issues.push(unknown(
                            format!("{prefix}.gate.{}", issue.key),
                            format!("{prefix}.gate.{sug}"),
                        ));
                    }
                }
            }
        }
    }
}

/// Flag an export arranged by a view its own surface does not declare.
///
/// The runtime refuses such an export outright (`prov-exports` fails closed
/// rather than falling back to the gate's whole set), so this is the
/// author-time half: reported here, the typo is fixed before the first
/// preview; unreported, it surfaces as a refusal at the moment someone tries
/// to publish, which is the worst time.
///
/// Only fires when `views` and `exports` are declared in the **same config
/// surface** — `diagnose` lints one surface at a time, the same bound every
/// other cross-key check here has.
fn diagnose_export_view_is_declared(
    issues: &mut Vec<ConfigIssue>,
    prefix: &str,
    spec: &Value,
    surface: &Mapping,
) {
    let Some(named) = spec.get("view").and_then(Value::as_str).map(str::trim) else {
        return;
    };
    let Some(views) = surface
        .get(prov_views::VIEWS_KEY)
        .and_then(Value::as_mapping)
    else {
        return;
    };
    if named.is_empty() || views.contains_key(named) {
        return;
    }
    let declared: Vec<String> = views.keys().cloned().collect();
    issues.push(ConfigIssue {
        key: format!("{prefix}.view"),
        kind: ConfigIssueKind::InvalidValue {
            value: named.to_string(),
            expected: declared,
        },
    });
}

/// Flag a block key whose value is not a mapping (e.g. `references: markdown`).
fn block_shape_issue(issues: &mut Vec<ConfigIssue>, key: &str, value: &Value) {
    issues.push(ConfigIssue {
        key: key.to_string(),
        kind: ConfigIssueKind::InvalidValue {
            value: value_summary(value),
            expected: vec!["a block of keys".into()],
        },
    });
}

/// Check an enum-valued axis, pushing an `InvalidValue` (with the accepted
/// spellings) when the written value does not parse.
fn enum_axis(
    issues: &mut Vec<ConfigIssue>,
    key: &str,
    value: &Value,
    parses: impl Fn(&str) -> bool,
    expected: &[&str],
) {
    if !value.as_str().is_some_and(parses) {
        issues.push(ConfigIssue {
            key: key.to_string(),
            kind: ConfigIssueKind::InvalidValue {
                value: value_summary(value),
                expected: expected.iter().map(|s| s.to_string()).collect(),
            },
        });
    }
}

/// Check a bool-valued axis.
fn bool_axis(issues: &mut Vec<ConfigIssue>, key: &str, value: &Value) {
    if value.as_bool().is_none() {
        issues.push(ConfigIssue {
            key: key.to_string(),
            kind: ConfigIssueKind::InvalidValue {
                value: value_summary(value),
                expected: vec!["true".into(), "false".into()],
            },
        });
    }
}

fn unknown(key: String, suggestion: String) -> ConfigIssue {
    ConfigIssue {
        key,
        kind: ConfigIssueKind::UnknownKey { suggestion },
    }
}

/// The `metadata.format` spellings compiled into this build (yaml is always
/// available; the rest are feature-gated, matching [`format_from_str`]).
fn embed_format_spellings() -> Vec<&'static str> {
    // `mut` is used only when a format feature below is compiled in.
    #[allow(unused_mut)]
    let mut v = vec!["yaml"];
    #[cfg(feature = "json")]
    v.push("json");
    #[cfg(feature = "toml")]
    v.push("toml");
    #[cfg(feature = "fig-lang")]
    v.push("fig");
    v
}

/// A short, human-readable rendering of a config value for a diagnostic message.
fn value_summary(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        _ => "(non-scalar)".to_string(),
    }
}

/// Parse a `metadata.format` config value (`yaml`/`json`/`toml`/`fig`) into a
/// metadata [`fig::Format`], honoring the compiled-in formats — the public form of
/// [`format_from_str`], for callers that name a frontmatter language from outside
/// the config parser (the CLI's `convert … metadata.format …`).
pub fn metadata_format_from_str(value: &str) -> Option<fig::Format> {
    format_from_str(value)
}

/// The `metadata.format` config spelling for a metadata [`fig::Format`] — the
/// public form of [`format_str`], and the inverse of [`metadata_format_from_str`].
pub fn metadata_format_str(format: fig::Format) -> &'static str {
    format_str(format)
}

/// Parse the `metadata.format` config value into a metadata format (only the
/// compiled-in formats are recognized; others → `None`, keeping the default).
fn format_from_str(value: &str) -> Option<fig::Format> {
    match value {
        "yaml" | "yml" => Some(fig::Format::Yaml),
        #[cfg(feature = "json")]
        "json" => Some(fig::Format::Json),
        #[cfg(feature = "toml")]
        "toml" => Some(fig::Format::Toml),
        #[cfg(feature = "fig-lang")]
        "fig" => Some(fig::Format::Fig),
        _ => None,
    }
}

/// The `metadata.format` config spelling for a metadata format.
fn format_str(format: fig::Format) -> &'static str {
    match format {
        #[cfg(feature = "json")]
        fig::Format::Json => "json",
        #[cfg(feature = "toml")]
        fig::Format::Toml => "toml",
        #[cfg(feature = "fig-lang")]
        fig::Format::Fig => "fig",
        _ => "yaml",
    }
}

/// Parse a relation `cardinality` config value (`one`/`many`); unknown → `None`.
fn cardinality_from_str(value: &str) -> Option<Cardinality> {
    match value {
        "one" => Some(Cardinality::One),
        "many" => Some(Cardinality::Many),
        _ => None,
    }
}

/// The `cardinality` config spelling for a [`Cardinality`].
fn cardinality_str(cardinality: Cardinality) -> &'static str {
    match cardinality {
        Cardinality::One => "one",
        Cardinality::Many => "many",
    }
}

/// Parse the `identity` config value into a registration trigger set. `none` is
/// the canonical spelling for "identity off" (see `docs/config-vocab.md`), but
/// `off` is accepted as a synonym so the two never diverge: it is the word the
/// CLI's `--identity` flag and every other "off" axis (`fixity: off`) use, and a
/// user who reaches for it must not be told it is invalid.
fn registration_from_str(value: &str) -> Option<Registration> {
    match value {
        "none" | "off" => Some(Registration::OFF),
        "lazy" => Some(Registration::LAZY),
        "eager" => Some(Registration::EAGER),
        _ => None,
    }
}

/// The `identity` config spelling for a registration trigger set. A custom
/// combination (not one of the three presets) is reported as its nearest name.
fn registration_str(registration: Registration) -> &'static str {
    match registration {
        Registration::OFF => "none",
        Registration::EAGER => "eager",
        _ => "lazy",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov_identity::Trigger;

    /// A config surface as a `Value::Mapping` from `(key, value)` pairs, values
    /// inferred as bools where they parse.
    fn config_doc(pairs: &[(&str, &str)]) -> Value {
        let mut map = Mapping::new();
        for (k, v) in pairs {
            let value = match *v {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                other => Value::String(other.into()),
            };
            map.insert((*k).into(), value);
        }
        Value::Mapping(map)
    }

    // Uses YAML frontmatter fixtures, so it runs under the `yaml` feature.
    #[test]
    #[cfg(feature = "yaml")]
    fn relation_set_builds_a_custom_vocabulary_and_falls_back_to_diaryx() {
        use prov_graph::document::Document;

        fn doc(text: &str) -> Document {
            Document::parse("index.md", text).unwrap()
        }

        // No relation defs → the diaryx preset unchanged (graceful degradation).
        let default_set = WorkspaceConfig::default().relation_set();
        assert_eq!(default_set.spanning_relation(), Some("contents"));
        assert_eq!(default_set.registry_relation(), Some("registry"));

        // Declared defs → a self-described `part`/`whole` vocabulary, still with
        // the structural pointer relations preserved.
        let config = WorkspaceConfig {
            spanning: Some("part".into()),
            relation_defs: BTreeMap::from([
                (
                    "part".to_string(),
                    RelationDef {
                        cardinality: Some(Cardinality::Many),
                        inverse: Some("whole".to_string()),
                        means: None,
                    },
                ),
                (
                    "whole".to_string(),
                    RelationDef {
                        cardinality: Some(Cardinality::One),
                        inverse: Some("part".to_string()),
                        means: None,
                    },
                ),
            ]),
            ..WorkspaceConfig::default()
        };
        let set = config.relation_set();
        assert_eq!(set.spanning_relation(), Some("part"));
        let d = doc("---\npart:\n- one.md\n- two.md\n---\nbody\n");
        assert_eq!(
            set.children(&fig::Value::from(&d.meta)),
            vec!["one.md".to_string(), "two.md".to_string()]
        );
        // Pointer relations survive a custom vocabulary so registry/config/bin
        // stay reachable.
        assert_eq!(set.registry_relation(), Some("registry"));
        assert!(set.relations().iter().any(|r| r.name == "recycle_bin"));
        assert_eq!(set.history_relation(), Some("history"));
        assert!(set.relations().iter().any(|r| r.name == "history"));
        assert_eq!(set.about_relation(), Some("about"));
        assert!(set.relations().iter().any(|r| r.name == "about"));
    }

    #[test]
    fn presets_encode_the_two_styles() {
        // Diaryx: no identity, path addressing. Obsidian: identity + id addressing.
        assert_eq!(WorkspaceConfig::paths_only().identity, Registration::OFF);
        assert_eq!(
            WorkspaceConfig::paths_only().reference_target,
            Addressing::Path
        );
        assert!(
            WorkspaceConfig::stable_ids()
                .identity
                .fires_on(Trigger::Link)
        );
        assert_eq!(
            WorkspaceConfig::stable_ids().reference_target,
            Addressing::Id
        );
    }

    #[test]
    fn round_trips_through_a_nested_mapping() {
        let config = WorkspaceConfig {
            identity: Registration::EAGER,
            notation: Notation::Bare,
            path_style: PathStyle::Relative,
            reference_target: Addressing::Id,
            reference_label: true,
            relation_styles: BTreeMap::from([
                (
                    "contents".to_string(),
                    RelationStyleConfig {
                        notation: Some(Notation::Wikilink),
                        path_style: None,
                        target: Some(Addressing::Alias),
                        label: None,
                    },
                ),
                (
                    "part_of".to_string(),
                    RelationStyleConfig {
                        notation: Some(Notation::Markdown),
                        path_style: Some(PathStyle::Relative),
                        target: Some(Addressing::Id),
                        label: Some(false),
                    },
                ),
            ]),
            spanning: Some("contents".to_string()),
            relation_defs: BTreeMap::from([
                (
                    "contents".to_string(),
                    RelationDef {
                        cardinality: Some(Cardinality::Many),
                        inverse: Some("part_of".to_string()),
                        means: Some("documents contained by this one".to_string()),
                    },
                ),
                (
                    "part_of".to_string(),
                    RelationDef {
                        cardinality: Some(Cardinality::One),
                        inverse: Some("contents".to_string()),
                        means: None,
                    },
                ),
            ]),
            fields: BTreeMap::from([
                (
                    "audience".to_string(),
                    FieldSpec {
                        ty: Some(FieldType::Str),
                        values: OpenClosed::Closed,
                        vocabulary: Some("[Audiences](/vocab/audiences.yaml)".to_string()),
                        reify: true,
                    },
                ),
                // A type with no vocabulary — the other half of a field
                // declaration, and the shape that has no `values` to write.
                (
                    "created".to_string(),
                    FieldSpec {
                        ty: Some(FieldType::Extended(ExtKind::LocalDate)),
                        values: OpenClosed::default(),
                        vocabulary: None,
                        reify: false,
                    },
                ),
            ]),
            views: vec![
                // A scoped, materializing view with a fallback chain — every
                // optional key populated, so nothing survives the round trip by
                // being absent at both ends.
                ViewSpec {
                    name: "daily".to_string(),
                    label: Some("Daily".to_string()),
                    icon: Some("calendar".to_string()),
                    group: prov_views::Grouping {
                        keys: vec!["date_of_document".to_string(), "created".to_string()],
                        by: Some(prov_views::Grain::Month),
                    },
                    under: Some("[Daily](id:abc1234)".to_string()),
                    // A condition too, so the round trip covers `where:`.
                    filter: Some(prov_views::Condition::Not(Box::new(
                        prov_views::Condition::Has("draft".to_string()),
                    ))),
                    nest: Some(prov_views::Grain::Year),
                },
                // …and the minimal one, which must not gain keys on the way
                // back.
                ViewSpec {
                    name: "who".to_string(),
                    label: None,
                    icon: None,
                    group: prov_views::Grouping::field("people"),
                    under: None,
                    filter: None,
                    nest: None,
                },
            ],
            exports: vec![
                // Every optional key populated, and the minimal form, for the
                // same reason as the two views above.
                ExportSpec {
                    name: "letters".to_string(),
                    label: Some("Letters home".to_string()),
                    gate: prov_exports::Gate {
                        field: "audience".to_string(),
                        value: "family".to_string(),
                    },
                    view: Some("daily".to_string()),
                },
                ExportSpec {
                    name: "notes".to_string(),
                    label: None,
                    gate: prov_exports::Gate {
                        field: "audience".to_string(),
                        value: "public".to_string(),
                    },
                    view: None,
                },
            ],
            id_storage: IdStorage::Frontmatter,
            default_embed_format: fig::Format::Yaml,
            embed_style: EmbedStyle::CodeBlock,
            content_format: ContentFormat::Djot,
            recycle_bin: false,
            fixity: Fixity::Full,
            // Non-default, so the round trip actually exercises the axis.
            history: History::Manual,
            // Likewise non-default — `structure` is the default, so `off` is
            // what proves the value survives the mapping rather than being
            // silently re-defaulted on the way back.
            about: About::Off,
            updated: "modified".to_string(),
            // Non-default (the default is anonymous), so the round trip proves
            // the name survives rather than being silently dropped.
            workspace_id: "notes".to_string(),
        };
        let back = WorkspaceConfig::from_meta(&Value::Mapping(config.to_mapping()));
        assert_eq!(back, config);
    }

    #[test]
    fn per_relation_styles_resolve_over_the_workspace_default() {
        // The diaryx up≠down example: a workspace default target of `id`, with
        // `contents` (down) overridden to a nominal alias wikilink and `part_of`
        // (up) to a bare markdown id link — each partial overlaying the default.
        let mut cfg = WorkspaceConfig::default();
        cfg.apply(&config_doc_nested(
            &[("target", "id")],
            &[
                ("contents", &[("notation", "wikilink"), ("target", "alias")]),
                ("part_of", &[("target", "id")]),
            ],
        ));

        let styles = cfg.resolved_relation_styles();
        let down = styles.get("contents").expect("contents style");
        assert_eq!(down.wrapper, prov_graph::link::Wrapper::Wikilink);
        assert_eq!(down.addressing, Addressing::Alias);

        let up = styles.get("part_of").expect("part_of style");
        // Inherits the default notation (markdown), keeps its own id target.
        assert_eq!(up.wrapper, prov_graph::link::Wrapper::Markdown);
        assert_eq!(up.addressing, Addressing::Id);
    }

    /// Build a config value with a top-level `references` block and a `relations`
    /// block of per-relation overrides.
    fn config_doc_nested(
        references: &[(&str, &str)],
        relations: &[(&str, &[(&str, &str)])],
    ) -> Value {
        let mut top = Mapping::new();
        let mut refs = Mapping::new();
        for (k, v) in references {
            refs.insert((*k).into(), Value::String((*v).into()));
        }
        top.insert("references".into(), Value::Mapping(refs));
        let mut rels = Mapping::new();
        for (name, axes) in relations {
            let mut spec = Mapping::new();
            for (k, v) in *axes {
                spec.insert((*k).into(), Value::String((*v).into()));
            }
            rels.insert((*name).into(), Value::Mapping(spec));
        }
        top.insert("relations".into(), Value::Mapping(rels));
        Value::Mapping(top)
    }

    #[test]
    fn a_retired_canonical_path_style_is_reported_and_falls_back_to_root() {
        // The migration contract for a workspace still configured with the
        // retired value. Two things have to be true at once, and they pull in
        // opposite directions: the workspace must keep *loading* (an archive
        // that will not open because a setting was withdrawn is worse than the
        // setting), and it must not quietly keep resolving links the way the
        // broken style did.
        //
        // Falling back to `root` is what squares them. `canonical` emitted a
        // bare workspace-relative path that `resolve` reads directory-relative,
        // so it only ever resolved correctly from the workspace root; `root`
        // emits the same path with the leading slash that makes that reading
        // explicit, and resolves correctly from anywhere. `check` says so, and
        // `prov convert <root> link_format markdown_root -r` rewrites the
        // documents to match.
        let mut cfg = WorkspaceConfig::default();
        let mut refs = Mapping::new();
        refs.insert("path_style".into(), Value::String("canonical".into()));
        let mut top = Mapping::new();
        top.insert("references".into(), Value::Mapping(refs));
        let meta = Value::Mapping(top);

        cfg.apply(&meta);
        assert_eq!(cfg.path_style, PathStyle::Root, "the resolvable spelling");

        let issues = diagnose(&meta);
        assert!(
            issues.iter().any(|i| matches!(
                &i.kind,
                ConfigIssueKind::InvalidValue { value, expected }
                    if value.contains("canonical") && expected == &["root", "relative"]
            )),
            "{issues:?}"
        );
    }

    #[test]
    fn reference_axes_orthogonalize_notation_and_resolution() {
        // bare + relative renders a plain directory-relative path; wikilink wraps.
        let mut cfg = WorkspaceConfig::default();
        let mut refs = Mapping::new();
        refs.insert("notation".into(), Value::String("bare".into()));
        refs.insert("path_style".into(), Value::String("relative".into()));
        let mut top = Mapping::new();
        top.insert("references".into(), Value::Mapping(refs));
        cfg.apply(&Value::Mapping(top));
        assert_eq!(cfg.link_format(), LinkStyle::PlainRelative);
        assert_eq!(cfg.notation, Notation::Bare);
        assert_eq!(cfg.path_style, PathStyle::Relative);
    }

    #[test]
    fn apply_overlays_only_present_keys_so_the_config_document_wins() {
        let mut config = WorkspaceConfig::default();
        // Root block sets only content_format.
        config.apply(&config_doc(&[("content_format", "djot")]));
        assert_eq!(config.content_format, ContentFormat::Djot);
        assert_eq!(config.identity, Registration::LAZY, "identity untouched");
        // The config document then overrides identity; content_format preserved.
        config.apply(&config_doc(&[("identity", "none")]));
        assert_eq!(config.identity, Registration::OFF);
        assert_eq!(config.content_format, ContentFormat::Djot);
    }

    #[test]
    fn diagnose_is_silent_on_a_clean_config_and_on_user_fields() {
        let doc = config_doc(&[
            ("title", "prov config"),
            ("part_of", "index.md"),
            ("id", "abc123"),
            ("spec", "1"),
            ("identity", "lazy"),
            ("fixity", "all"),
            ("recycle_bin", "false"),
            ("content_format", "djot"),
            ("id_storage", "both"),
            ("author", "someone"),
        ]);
        assert!(diagnose(&doc).is_empty(), "flagged: {:?}", diagnose(&doc));
    }

    #[test]
    fn diagnose_flags_a_misspelled_top_level_key_with_a_suggestion() {
        let issues = diagnose(&config_doc(&[("recyle_bin", "false")]));
        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].kind,
            ConfigIssueKind::UnknownKey {
                suggestion: "recycle_bin".into()
            }
        );
    }

    #[test]
    fn workspace_id_applies_when_well_formed_and_is_ignored_when_not() {
        let mut cfg = WorkspaceConfig::default();
        assert_eq!(cfg.workspace_id, "", "anonymous by default");

        cfg.apply(&config_doc(&[("workspace_id", "notes")]));
        assert_eq!(cfg.workspace_id, "notes");

        // A malformed value never half-lands: the previous name stands rather
        // than being replaced by something prov cannot write into a reference.
        for bad in ["with/slash", "with:colon", "with space", ""] {
            cfg.apply(&config_doc(&[("workspace_id", bad)]));
            assert_eq!(cfg.workspace_id, "notes", "rejected {bad:?}");
        }
    }

    #[test]
    fn diagnose_flags_a_malformed_workspace_id_but_not_an_empty_one() {
        for bad in ["with/slash", "with:colon", "with space"] {
            let issues = diagnose(&config_doc(&[("workspace_id", bad)]));
            assert_eq!(
                issues.first().map(|i| &i.kind),
                Some(&ConfigIssueKind::MalformedWorkspaceId {
                    value: bad.to_string()
                }),
                "{bad:?}"
            );
        }
        // Empty is the explicit spelling of anonymous — the same shape as an
        // empty `updated` — so it is clean, and `to_mapping` may write it.
        assert!(
            diagnose(&config_doc(&[("workspace_id", "")])).is_empty(),
            "an empty name is anonymity, not an error"
        );
        assert!(diagnose(&config_doc(&[("workspace_id", "notes")])).is_empty());
    }

    #[test]
    fn diagnose_flags_bad_values_and_typos_inside_nested_blocks() {
        // references.notaton (typo) + references.target bad value.
        let mut refs = Mapping::new();
        refs.insert("notaton".into(), Value::String("markdown".into()));
        refs.insert("target".into(), Value::String("pointer".into()));
        let mut top = Mapping::new();
        top.insert("references".into(), Value::Mapping(refs));
        let issues = diagnose(&Value::Mapping(top));
        assert!(
            issues.iter().any(|i| i.key == "references.notaton"
                && matches!(&i.kind, ConfigIssueKind::UnknownKey { suggestion } if suggestion == "references.notation")),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|i| i.key == "references.target"
                && matches!(&i.kind, ConfigIssueKind::InvalidValue { value, .. } if value == "pointer")),
            "{issues:?}"
        );
    }

    #[test]
    fn diagnose_flags_an_unrecognized_value_on_a_real_key() {
        let issues = diagnose(&config_doc(&[("fixity", "alll")]));
        assert_eq!(issues.len(), 1);
        match &issues[0].kind {
            ConfigIssueKind::InvalidValue { value, expected } => {
                assert_eq!(value, "alll");
                assert!(expected.contains(&"all".to_string()), "{expected:?}");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn about_defaults_on_and_accepts_only_its_two_spellings() {
        // Default is `structure`, not `off` — the one axis that departs from
        // `history`'s posture, because self-description by default is the thesis.
        assert_eq!(WorkspaceConfig::default().about, About::Structure);
        assert!(About::Structure.generates());
        assert!(!About::Off.generates());

        let mut cfg = WorkspaceConfig::default();
        cfg.apply(&config_doc(&[("about", "off")]));
        assert_eq!(cfg.about, About::Off);

        // An unknown spelling is a finding that names both accepted values, and
        // leaves the default in place rather than guessing.
        let issues = diagnose(&config_doc(&[("about", "structrue")]));
        assert_eq!(issues.len(), 1);
        match &issues[0].kind {
            ConfigIssueKind::InvalidValue { value, expected } => {
                assert_eq!(value, "structrue");
                assert!(expected.contains(&"structure".to_string()), "{expected:?}");
                assert!(expected.contains(&"off".to_string()), "{expected:?}");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
        let mut unchanged = WorkspaceConfig::default();
        unchanged.apply(&config_doc(&[("about", "structrue")]));
        assert_eq!(unchanged.about, About::Structure);
    }

    #[test]
    fn relation_defs_and_spanning_apply_and_round_trip() {
        // A fully self-described `part`/`whole` vocabulary from config.
        let mut top = Mapping::new();
        top.insert("spanning".into(), Value::String("part".into()));
        let mut rels = Mapping::new();
        let mut part = Mapping::new();
        part.insert("cardinality".into(), Value::String("many".into()));
        part.insert("inverse".into(), Value::String("whole".into()));
        part.insert("means".into(), Value::String("the pieces".into()));
        let mut whole = Mapping::new();
        whole.insert("cardinality".into(), Value::String("one".into()));
        whole.insert("inverse".into(), Value::String("part".into()));
        rels.insert("part".into(), Value::Mapping(part));
        rels.insert("whole".into(), Value::Mapping(whole));
        top.insert("relations".into(), Value::Mapping(rels));

        let cfg = WorkspaceConfig::from_meta(&Value::Mapping(top));
        assert_eq!(cfg.spanning.as_deref(), Some("part"));
        let part_def = cfg.relation_defs.get("part").expect("part def");
        assert_eq!(part_def.cardinality, Some(Cardinality::Many));
        assert_eq!(part_def.inverse.as_deref(), Some("whole"));
        assert_eq!(part_def.means.as_deref(), Some("the pieces"));
        // A clean self-described vocabulary passes its own diagnosis.
        assert!(diagnose(&Value::Mapping(cfg.to_mapping())).is_empty());
    }

    #[test]
    fn diagnose_flags_a_spanning_relation_whose_inverse_is_many() {
        // `spanning: part`, but its inverse `whole` is declared `many` — that
        // cannot be a single-parent tree.
        let mut top = Mapping::new();
        top.insert("spanning".into(), Value::String("part".into()));
        let mut rels = Mapping::new();
        let mut part = Mapping::new();
        part.insert("inverse".into(), Value::String("whole".into()));
        let mut whole = Mapping::new();
        whole.insert("cardinality".into(), Value::String("many".into()));
        rels.insert("part".into(), Value::Mapping(part));
        rels.insert("whole".into(), Value::Mapping(whole));
        top.insert("relations".into(), Value::Mapping(rels));

        let issues = diagnose(&Value::Mapping(top));
        assert!(
            issues.iter().any(|i| i.key == "spanning"
                && matches!(&i.kind, ConfigIssueKind::SpanningNotSingleParent { inverse } if inverse == "whole")),
            "{issues:?}"
        );
    }

    /// A field declaration used to require a vocabulary to exist at all. A type
    /// is the other, independent half: `created` is a date that nothing controls.
    #[test]
    fn a_field_may_declare_a_type_without_a_vocabulary() {
        let mut created = Mapping::new();
        created.insert("type".into(), Value::String("date".into()));
        let mut fields = Mapping::new();
        fields.insert("created".into(), Value::Mapping(created));
        let mut top = Mapping::new();
        top.insert("fields".into(), Value::Mapping(fields));

        let config = WorkspaceConfig::from_meta(&Value::Mapping(top));
        let spec = config.fields.get("created").expect("a recorded field");
        assert_eq!(spec.ty, Some(FieldType::Extended(ExtKind::LocalDate)));
        assert_eq!(spec.vocabulary, None);
    }

    /// The inverse guard: an entry that declares neither is not a description of
    /// anything, so it is not recorded as one.
    #[test]
    fn a_field_declaring_neither_type_nor_vocabulary_is_not_recorded() {
        let mut empty = Mapping::new();
        empty.insert("reify".into(), Value::Bool(true));
        let mut fields = Mapping::new();
        fields.insert("mystery".into(), Value::Mapping(empty));
        let mut top = Mapping::new();
        top.insert("fields".into(), Value::Mapping(fields));

        let config = WorkspaceConfig::from_meta(&Value::Mapping(top));
        assert!(config.fields.is_empty(), "{:?}", config.fields);
    }

    /// A `views:` block, as a config surface writes it.
    fn views_block(entries: &[(&str, &[(&str, Value)])]) -> Value {
        let mut views = Mapping::new();
        for (name, keys) in entries {
            let mut entry = Mapping::new();
            for (k, v) in *keys {
                entry.insert((*k).into(), v.clone());
            }
            views.insert((*name).into(), Value::Mapping(entry));
        }
        let mut top = Mapping::new();
        top.insert("views".into(), Value::Mapping(views));
        Value::Mapping(top)
    }

    fn str_value(text: &str) -> Value {
        Value::String(text.to_string())
    }

    #[test]
    fn views_apply_in_declaration_order() {
        let config = WorkspaceConfig::from_meta(&views_block(&[
            ("daily", &[("group", str_value("created"))]),
            ("who", &[("group", str_value("people"))]),
        ]));
        assert_eq!(
            config
                .views
                .iter()
                .map(|v| v.name.as_str())
                .collect::<Vec<_>>(),
            ["daily", "who"]
        );
    }

    /// The same merge rule `fields` has, for the same reason: a vault config
    /// declaring one view must not wipe the ones an app's defaults supplied.
    /// Redeclaring a name replaces that view whole rather than merging into it —
    /// `by` means nothing without `group`, so a key-wise merge would build a
    /// view neither surface wrote.
    #[test]
    fn a_later_surface_replaces_one_view_and_leaves_the_others() {
        let mut config = WorkspaceConfig::from_meta(&views_block(&[
            (
                "daily",
                &[
                    ("group", str_value("created")),
                    ("by", str_value("month")),
                    ("icon", str_value("calendar")),
                ],
            ),
            ("who", &[("group", str_value("people"))]),
        ]));
        config.apply(&views_block(&[(
            "daily",
            &[("group", str_value("date_of_document"))],
        )]));

        assert_eq!(
            config
                .views
                .iter()
                .map(|v| v.name.as_str())
                .collect::<Vec<_>>(),
            ["daily", "who"],
            "position is kept, and the untouched view survives"
        );
        let daily = &config.views[0];
        assert_eq!(daily.group, prov_views::Grouping::field("date_of_document"));
        assert_eq!(daily.group.by, None, "replaced whole, not merged key-wise");
        assert_eq!(daily.icon, None);
    }

    /// An entry that says nothing about grouping is not a view — and, unlike a
    /// silently dropped one, it is reported.
    #[test]
    fn a_view_without_a_grouping_is_not_recorded_and_is_diagnosed() {
        let meta = views_block(&[("daily", &[("label", str_value("Daily"))])]);
        assert!(WorkspaceConfig::from_meta(&meta).views.is_empty());

        let issues = diagnose(&meta);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].key, "views.daily.group");
        assert!(matches!(
            &issues[0].kind,
            ConfigIssueKind::InvalidValue { value, .. } if value == "(absent)"
        ));
    }

    /// `ViewSpec::parse` reads an unparseable grain as no grain — it will not
    /// invent a cut the config did not ask for — so the view still works and
    /// the linter is the only thing that ever says the config was wrong.
    #[test]
    fn diagnose_flags_a_misspelled_grain_and_a_misspelled_view_key() {
        let issues = diagnose(&views_block(&[(
            "daily",
            &[
                ("group", str_value("created")),
                ("by", str_value("yearr")),
                ("labl", str_value("Daily")),
            ],
        )]));
        assert!(
            issues.iter().any(|i| i.key == "views.daily.by"
                && matches!(&i.kind, ConfigIssueKind::InvalidValue { value, expected }
                    if value == "yearr" && expected.iter().any(|e| e == "year"))),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|i| i.key == "views.daily.labl"
                && i.kind
                    == ConfigIssueKind::UnknownKey {
                        suggestion: "views.daily.label".into()
                    }),
            "{issues:?}"
        );
    }

    /// An `exports:` block, as a config surface writes it.
    fn exports_block(entries: &[(&str, &[(&str, Value)])]) -> Value {
        let mut exports = Mapping::new();
        for (name, keys) in entries {
            let mut entry = Mapping::new();
            for (k, v) in *keys {
                entry.insert((*k).into(), v.clone());
            }
            exports.insert((*name).into(), Value::Mapping(entry));
        }
        let mut top = Mapping::new();
        top.insert("exports".into(), Value::Mapping(exports));
        Value::Mapping(top)
    }

    fn gate_value(field: &str, value: &str) -> Value {
        let mut gate = Mapping::new();
        gate.insert("field".into(), str_value(field));
        gate.insert("value".into(), str_value(value));
        Value::Mapping(gate)
    }

    #[test]
    fn exports_apply_and_round_trip() {
        let config = WorkspaceConfig::from_meta(&exports_block(&[
            (
                "letters",
                &[
                    ("gate", gate_value("audience", "family")),
                    ("view", str_value("daily")),
                ],
            ),
            ("notes", &[("gate", gate_value("audience", "public"))]),
        ]));
        assert_eq!(
            config
                .exports
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["letters", "notes"]
        );
        assert_eq!(config.exports[0].gate.field, "audience");
        assert_eq!(config.exports[0].view.as_deref(), Some("daily"));

        let written = config.to_mapping();
        let reread = WorkspaceConfig::from_meta(&Value::Mapping(written));
        assert_eq!(reread.exports, config.exports);
    }

    /// The same whole-entry replacement `views` has, for a sharper reason: an
    /// export half-merged across two surfaces would bound what leaves with a
    /// gate neither surface wrote.
    #[test]
    fn a_later_surface_replaces_one_export_whole() {
        let mut config = WorkspaceConfig::from_meta(&exports_block(&[(
            "letters",
            &[
                ("gate", gate_value("audience", "family")),
                ("view", str_value("daily")),
            ],
        )]));
        config.apply(&exports_block(&[(
            "letters",
            &[("gate", gate_value("audience", "friends"))],
        )]));

        assert_eq!(config.exports.len(), 1);
        assert_eq!(config.exports[0].gate.value, "friends");
        assert_eq!(
            config.exports[0].view, None,
            "replaced whole, not merged key-wise"
        );
    }

    /// A dropped export publishes nothing, silently — the report is the only
    /// thing that ever says the declaration does not exist.
    #[test]
    fn an_export_without_a_gate_is_not_recorded_and_is_diagnosed() {
        let meta = exports_block(&[("letters", &[("view", str_value("daily"))])]);
        assert!(WorkspaceConfig::from_meta(&meta).exports.is_empty());

        let issues = diagnose(&meta);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].key, "exports.letters.gate");
        assert!(matches!(
            &issues[0].kind,
            ConfigIssueKind::InvalidValue { value, .. } if value == "(absent)"
        ));
    }

    #[test]
    fn diagnose_flags_misspelled_export_keys_at_both_levels() {
        let mut gate = Mapping::new();
        gate.insert("field".into(), str_value("audience"));
        gate.insert("valeu".into(), str_value("family"));
        let issues = diagnose(&exports_block(&[(
            "letters",
            &[("gate", Value::Mapping(gate)), ("veiw", str_value("daily"))],
        )]));
        assert!(
            issues.iter().any(|i| i.kind
                == ConfigIssueKind::UnknownKey {
                    suggestion: "exports.letters.view".into()
                }),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|i| i.kind
                == ConfigIssueKind::UnknownKey {
                    suggestion: "exports.letters.gate.value".into()
                }),
            "{issues:?}"
        );
    }

    /// The runtime refuses an export whose view nobody declares (fail closed);
    /// this is the author-time half, so the typo is fixed before the first
    /// preview rather than at the moment someone tries to publish.
    #[test]
    fn diagnose_flags_an_export_arranged_by_an_undeclared_view() {
        let mut top = Mapping::new();
        let Value::Mapping(views) = views_block(&[("daily", &[("group", str_value("created"))])])
        else {
            unreachable!()
        };
        let Value::Mapping(exports) = exports_block(&[(
            "letters",
            &[
                ("gate", gate_value("audience", "family")),
                ("view", str_value("dialy")),
            ],
        )]) else {
            unreachable!()
        };
        for (k, v) in views.iter().chain(exports.iter()) {
            top.insert(k.clone(), v.clone());
        }

        let issues = diagnose(&Value::Mapping(top));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].key, "exports.letters.view");
        assert!(
            matches!(
                &issues[0].kind,
                ConfigIssueKind::InvalidValue { value, expected }
                    if value == "dialy" && expected == &vec!["daily".to_string()]
            ),
            "{issues:?}"
        );

        // And the same export beside no `views:` block is silent — one
        // surface at a time, the bound every cross-key check here has.
        let issues = diagnose(&exports_block(&[(
            "letters",
            &[
                ("gate", gate_value("audience", "family")),
                ("view", str_value("dialy")),
            ],
        )]));
        assert!(issues.is_empty(), "{issues:?}");
    }

    /// `nest` files into the single-parent spine, so a document with two values
    /// for the grouping field has two homes. Grouping by it is fine — only the
    /// filing half is reported.
    #[test]
    fn diagnose_flags_a_nest_on_a_multi_valued_field() {
        let block = |view: &[(&str, Value)]| {
            let mut fields = Mapping::new();
            let mut people = Mapping::new();
            people.insert("type".into(), str_value("seq"));
            fields.insert("people".into(), Value::Mapping(people));

            let mut views = Mapping::new();
            let mut entry = Mapping::new();
            for (k, v) in view {
                entry.insert((*k).into(), v.clone());
            }
            views.insert("who".into(), Value::Mapping(entry));

            let mut top = Mapping::new();
            top.insert("fields".into(), Value::Mapping(fields));
            top.insert("views".into(), Value::Mapping(views));
            Value::Mapping(top)
        };

        let issues = diagnose(&block(&[
            ("group", str_value("people")),
            ("nest", str_value("initial")),
        ]));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].key, "views.who.nest");
        assert_eq!(
            issues[0].kind,
            ConfigIssueKind::NestNotSingleValued {
                field: "people".into()
            }
        );

        // The same view without `nest:` is clean — one document under several
        // groups is what a view is *for*.
        assert!(
            diagnose(&block(&[("group", str_value("people"))])).is_empty(),
            "grouping by a multi-valued field is not the problem"
        );
    }

    /// The bound worth knowing: `diagnose` lints one surface at a time, so the
    /// cross-key check is silent when `fields` and `views` are declared apart.
    #[test]
    fn the_nest_check_is_silent_across_two_config_surfaces() {
        let mut views = Mapping::new();
        let mut entry = Mapping::new();
        entry.insert("group".into(), str_value("people"));
        entry.insert("nest".into(), str_value("initial"));
        views.insert("who".into(), Value::Mapping(entry));
        let mut top = Mapping::new();
        top.insert("views".into(), Value::Mapping(views));

        assert!(
            diagnose(&Value::Mapping(top)).is_empty(),
            "no `fields` in this surface to contradict it"
        );
    }

    #[test]
    fn diagnose_flags_a_views_block_that_is_not_a_block() {
        let mut top = Mapping::new();
        top.insert("views".into(), Value::String("daily".into()));
        let issues = diagnose(&Value::Mapping(top));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].key, "views");

        let issues = diagnose(&views_block(&[]));
        assert!(issues.is_empty(), "an empty block is clean: {issues:?}");
    }

    #[test]
    fn every_field_type_spelling_round_trips() {
        for spelling in FIELD_TYPES {
            let ty = field_type_from_config_str(spelling)
                .unwrap_or_else(|| panic!("{spelling} is offered but does not parse"));
            assert_eq!(field_type_as_config_str(ty), Some(*spelling));
        }
    }

    #[test]
    fn diagnose_flags_an_unknown_field_type_and_offers_the_near_miss() {
        let mut created = Mapping::new();
        created.insert("type".into(), Value::String("datetime2".into()));
        let mut fields = Mapping::new();
        fields.insert("created".into(), Value::Mapping(created));
        let mut top = Mapping::new();
        top.insert("fields".into(), Value::Mapping(fields));

        let issues = diagnose(&Value::Mapping(top));
        assert!(
            issues.iter().any(|i| i.key == "fields.created.type"
                && matches!(
                    &i.kind,
                    ConfigIssueKind::InvalidValue { expected, .. }
                        if expected.iter().any(|e| e == "datetime")
                )),
            "{issues:?}"
        );
    }

    #[test]
    fn diagnose_flags_bad_field_and_relation_def_values() {
        // fields.audience.values bad + a relations def with bad cardinality.
        let mut top = Mapping::new();
        let mut fields = Mapping::new();
        let mut audience = Mapping::new();
        audience.insert("values".into(), Value::String("secret".into())); // not open/closed
        audience.insert("vocabulary".into(), Value::String("/vocab/aud.yaml".into()));
        fields.insert("audience".into(), Value::Mapping(audience));
        top.insert("fields".into(), Value::Mapping(fields));
        let mut rels = Mapping::new();
        let mut c = Mapping::new();
        c.insert("cardinality".into(), Value::String("two".into())); // not one/many
        rels.insert("contents".into(), Value::Mapping(c));
        top.insert("relations".into(), Value::Mapping(rels));

        let issues = diagnose(&Value::Mapping(top));
        assert!(
            issues.iter().any(|i| i.key == "fields.audience.values"),
            "{issues:?}"
        );
        assert!(
            issues
                .iter()
                .any(|i| i.key == "relations.contents.cardinality"),
            "{issues:?}"
        );
    }

    #[test]
    fn spec_ahead_fires_only_for_a_newer_spec() {
        assert_eq!(
            spec_ahead(&config_doc(&[("identity", "lazy")])),
            None,
            "absent spec"
        );
        let at = {
            let mut m = Mapping::new();
            m.insert("spec".into(), Value::Int(SPEC_VERSION));
            Value::Mapping(m)
        };
        assert_eq!(spec_ahead(&at), None, "current spec is fine");
        let ahead = {
            let mut m = Mapping::new();
            m.insert("spec".into(), Value::Int(SPEC_VERSION + 1));
            Value::Mapping(m)
        };
        assert_eq!(spec_ahead(&ahead), Some(SPEC_VERSION + 1));
    }

    #[test]
    fn serialized_defaults_and_presets_all_pass_diagnosis() {
        for config in [
            WorkspaceConfig::default(),
            WorkspaceConfig::paths_only(),
            WorkspaceConfig::stable_ids(),
        ] {
            let serialized = Value::Mapping(config.to_mapping());
            assert!(
                diagnose(&serialized).is_empty(),
                "flagged itself: {:?}",
                diagnose(&serialized)
            );
        }
    }
}
