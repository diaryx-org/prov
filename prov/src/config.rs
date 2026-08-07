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

use crate::content::ContentFormat;
use crate::document::EmbedStyle;
use crate::identity::Registration;
use crate::link::{Addressing, LinkStyle, Notation, PathStyle, ReferenceStyle};
use crate::meta::{Mapping, Value};
use crate::relation::Cardinality;
use crate::textdist::nearest;

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
/// [`Relation::style`](crate::relation::Relation::style), and what lets links
/// going "down" (`contents`) differ from links going "up" (`part_of`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelationStyleConfig {
    /// The notation override (`markdown` / `wikilink` / `bare`).
    pub notation: Option<Notation>,
    /// The path-resolution override (`root` / `relative` / `canonical`).
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
    })
}

/// Where a document's stable ID is persisted — the identity-storage axis
/// (DESIGN §5). Orthogonal to *when* an ID is minted ([`Registration`]) and to
/// how references are spelled; this is purely the ID's *home*.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IdStorage {
    /// **Registry only** (`registry`): IDs live solely in the registry document —
    /// authoritative, non-derivable, resolved by direct lookup. The cleanest
    /// documents (no `id` clutter), but identity does not travel with a file.
    Registry,
    /// **Frontmatter + registry** (`both`, the default): each document also
    /// carries its own ID in an `id` frontmatter field (a portable, self-describing
    /// shadow), and the registry is retained as a rebuildable cache + tombstone
    /// ledger. The ID travels with the file across copies and out-of-band moves.
    #[default]
    Frontmatter,
    /// **Frontmatter only** (`frontmatter`): the `id` field is the sole home; no
    /// registry document is written and resolution rebuilds the id→path map by
    /// scanning frontmatter. Maximally self-describing, but it forfeits tombstones
    /// (a deleted file takes its ID with it), so an ID can in principle be reminted.
    FrontmatterOnly,
}

impl IdStorage {
    /// Whether this mode writes the ID into each document's `id` frontmatter.
    pub fn stamps_frontmatter(self) -> bool {
        matches!(self, IdStorage::Frontmatter | IdStorage::FrontmatterOnly)
    }

    /// Whether this mode keeps a registry document (the authoritative store, or —
    /// under [`Frontmatter`](IdStorage::Frontmatter) — a rebuildable cache).
    pub fn keeps_registry(self) -> bool {
        matches!(self, IdStorage::Registry | IdStorage::Frontmatter)
    }

    /// Parse the `id_storage` config spelling; unknown → `None`. `both` is the
    /// frontmatter+registry default; `frontmatter` is the registry-less mode.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "registry" => Some(Self::Registry),
            "both" => Some(Self::Frontmatter),
            "frontmatter" => Some(Self::FrontmatterOnly),
            _ => None,
        }
    }

    /// The `id_storage` config spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Registry => "registry",
            Self::Frontmatter => "both",
            Self::FrontmatterOnly => "frontmatter",
        }
    }
}

/// How far content-checksum (fixity) coverage extends — the archival integrity
/// axis. Orthogonal to the identity and link axes; this is purely about
/// detecting bit-rot in stored bytes.
///
/// The tiers exist because fixity means different things for different content.
/// An **attachment** is never edited, so a change to its bytes is unambiguously
/// corruption — safe to checksum by default, with no friction. A **document
/// body** *is* edited, and a legitimate external edit is indistinguishable from
/// rot to a checker, so hashing bodies is opt-in and best paired with
/// `prov edit` (which restamps on save). Frontmatter is never hashed: it is
/// small, structured, edited constantly by prov's own link maintenance, and
/// its corruption already surfaces as parse or link findings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Fixity {
    /// No content checksums are recorded or verified (`off`).
    Off,
    /// **Attachments only** (`attachments`, the default): each attachment sidecar
    /// records a `content_hash` of its payload, and `check` verifies it.
    /// Unambiguous — a payload's bytes changing is always corruption — so there is
    /// no edit friction and nothing to opt out of per document.
    #[default]
    Payloads,
    /// **Attachments and document bodies** (`all`): additionally, each document
    /// records a `content_hash` of its *body* (never its frontmatter). The
    /// archival-grade tier; because a body is editable, pair it with
    /// `prov edit` so a body change restamps the hash, and treat an
    /// out-of-band edit as a `check` finding to re-bless rather than a hard error.
    Full,
}

impl Fixity {
    /// Whether attachment payloads are checksummed (true for every tier but off).
    pub fn covers_payloads(self) -> bool {
        matches!(self, Fixity::Payloads | Fixity::Full)
    }

    /// Whether document bodies are checksummed (only the `all` tier).
    pub fn covers_bodies(self) -> bool {
        matches!(self, Fixity::Full)
    }

    /// Parse the `fixity` config spelling; unknown → `None`.
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "attachments" => Some(Self::Payloads),
            "all" => Some(Self::Full),
            _ => None,
        }
    }

    /// The `fixity` config spelling.
    pub fn as_config_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Payloads => "attachments",
            Self::Full => "all",
        }
    }
}

/// Whether the workspace keeps a **history store** — one immutable event
/// document per capture, plus content-addressed pre-image blobs — and how a
/// capture is triggered.
///
/// The feature is a safety net for *structural* damage introduced by an external
/// sync transport: a rename, move or delete touches several files at once, and a
/// transport reconciling bytes with no idea about prov's graph can produce a
/// clean-looking merge that is semantically broken. An event is a consistent cut
/// across every file it captured together, so restoring one puts the set back.
///
/// Default **off**, unlike `recycle_bin`: history adds ongoing storage the user
/// has not asked for, and a manual-only trigger buys nothing until the user is in
/// the habit anyway. It is also the wrong tool when the transport is **git**,
/// which already stores every pre-image, dedupes by content, and reconciles
/// concurrent histories — the feature earns its keep on Dropbox, Syncthing,
/// iCloud, a synced network share.
///
/// `off` gates *capture* only. The read and recovery verbs work regardless:
/// recovery must never be gated behind re-enabling a setting, least of all on the
/// machine that just suffered the damage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum History {
    /// No history store is maintained (`off`, the default). `history-capture`
    /// refuses; an existing store is still readable, restorable and validated.
    #[default]
    Off,
    /// Captures happen when the user asks (`manual`) — `prov history-capture`,
    /// run by hand or by a pre-sync script the user wires up themselves. prov
    /// does not run the sync, so there is no event for it to hook.
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
    /// Whether `history-capture` is permitted to write a new event.
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
    /// Overridden per relation by [`Relation::style`](crate::relation::Relation::style).
    pub notation: Notation,
    /// The default **path resolution** for path targets (`root` / `relative` /
    /// `canonical`). Ignored for id/alias targets.
    pub path_style: PathStyle,
    /// The default reference **addressing** (`path` / `id` / `alias`).
    pub reference_target: Addressing,
    /// Whether an id/alias reference carries a `|Title` label.
    pub reference_label: bool,
    /// Per-relation reference-style overrides, keyed by relation name — the
    /// config form of [`Relation::style`](crate::relation::Relation::style).
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
    /// (diaryx) unchanged. Consumed by
    /// [`RelationSet::from_config`](crate::relation::RelationSet::from_config).
    pub relation_defs: BTreeMap<String, RelationDef>,
    /// Controlled-vocabulary field declarations, keyed by frontmatter field name
    /// (`tags`, `audience`). Empty means no field is controlled — every such
    /// field is ordinary carried content (DESIGN §2, tier 3).
    pub fields: BTreeMap<String, FieldSpec>,
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
    /// [`Target::Foreign`](crate::workspace::Target::Foreign).
    ///
    /// Must be [well-formed](is_valid_workspace_id): a malformed value is
    /// reported by [`diagnose`] and ignored rather than half-honored.
    pub workspace_id: String,
}

/// Whether `name` is a usable workspace self-name.
///
/// The constraint comes entirely from where the name is *written*: it is the
/// qualifier in an `id:<workspace>/<id>` target, so it may not contain the `/`
/// that separates it from the id, the `:` that ends the scheme, or whitespace
/// (a target is a single scalar; a space would make it two). Anything else is
/// the user's business — this is a name for humans to choose, not an opaque
/// handle prov mints.
///
/// Deliberately *not* checked: uniqueness across workspaces. Nothing here can
/// see another workspace, so a collision is undetectable from inside; it is the
/// resolving host's problem, and the host is the only thing that has the
/// evidence to notice.
pub fn is_valid_workspace_id(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c == '/' || c == ':' || c.is_whitespace())
}

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
    /// [`RelationSet::with_styles`](crate::relation::RelationSet::with_styles) to
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
        (link_registers && self.identity.fires_on(crate::identity::Trigger::Link))
            || self.identity.fires_on(crate::identity::Trigger::Create)
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
                &["root", "relative", "canonical"],
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
                &["root", "relative", "canonical"],
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
    use crate::identity::Trigger;

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
            path_style: PathStyle::Canonical,
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
        assert_eq!(down.wrapper, crate::link::Wrapper::Wikilink);
        assert_eq!(down.addressing, Addressing::Alias);

        let up = styles.get("part_of").expect("part_of style");
        // Inherits the default notation (markdown), keeps its own id target.
        assert_eq!(up.wrapper, crate::link::Wrapper::Markdown);
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
    fn reference_axes_orthogonalize_notation_and_resolution() {
        // bare + canonical renders a plain workspace-relative path; wikilink wraps.
        let mut cfg = WorkspaceConfig::default();
        let mut refs = Mapping::new();
        refs.insert("notation".into(), Value::String("bare".into()));
        refs.insert("path_style".into(), Value::String("canonical".into()));
        let mut top = Mapping::new();
        top.insert("references".into(), Value::Mapping(refs));
        cfg.apply(&Value::Mapping(top));
        assert_eq!(cfg.link_format(), LinkStyle::PlainCanonical);
        assert_eq!(cfg.notation, Notation::Bare);
        assert_eq!(cfg.path_style, PathStyle::Canonical);
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
