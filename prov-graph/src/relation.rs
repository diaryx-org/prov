//! Relations — the configurable vocabulary of links declared in metadata.
//!
//! prov is opinionated about the *mechanism* (links live in embedded
//! metadata; one relation is the canonical tree; the rest overlay it) but not
//! about the *vocabulary*. A [`RelationSet`] names which fields are links, their
//! cardinality, their inverse, and which single relation is **spanning**.

use crate::link::ReferenceStyle;

/// How many targets a relation field may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    /// At most one target (e.g. a single-parent `part_of`).
    One,
    /// Any number of targets (e.g. `contents`, `links`).
    Many,
}

/// A single named relation: the frontmatter key it reads, its inverse (if the
/// pair is maintained bidirectionally), and its cardinality.
#[derive(Debug, Clone)]
pub struct Relation {
    /// The frontmatter key this relation reads (e.g. `"contents"`).
    pub name: String,
    /// The inverse relation's name, if any (e.g. `contents` ↔ `part_of`).
    pub inverse: Option<String>,
    /// How many targets the field may hold.
    pub cardinality: Cardinality,
    /// The reference style prov authors *this* relation's links in,
    /// overriding the workspace default. `None` inherits the default. This is
    /// what lets links going "down" (`contents`) differ from links going "up"
    /// (`part_of`) — style is resolved per relation (see
    /// `docs/reference-styles.md`).
    pub style: Option<ReferenceStyle>,
}

impl Relation {
    /// A single-valued relation (cardinality [`Cardinality::One`]).
    pub fn one(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            inverse: None,
            cardinality: Cardinality::One,
            style: None,
        }
    }

    /// A multi-valued relation (cardinality [`Cardinality::Many`]).
    pub fn many(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            inverse: None,
            cardinality: Cardinality::Many,
            style: None,
        }
    }

    /// Declare this relation's inverse (builder-style).
    pub fn inverse(mut self, name: impl Into<String>) -> Self {
        self.inverse = Some(name.into());
        self
    }

    /// Author this relation's links in a specific reference style, overriding
    /// the workspace default (builder-style). E.g. `alias` wikilinks going down
    /// through `contents`, durable `id` links going up through `part_of`.
    pub fn style(mut self, style: ReferenceStyle) -> Self {
        self.style = Some(style);
        self
    }
}

/// A resolved link found in a document's metadata: which relation declared it
/// and the raw (unresolved) target string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// The relation (frontmatter key) that declared this link.
    pub relation: String,
    /// The raw target string exactly as written in the metadata.
    pub target: String,
}

/// The configured set of relations for a workspace, and which one is spanning.
///
/// The **spanning** relation is the single-parent containment tree that gives
/// the workspace its self-describing discovery spine. All other relations may
/// be many-to-many overlays.
#[derive(Debug, Clone, Default)]
pub struct RelationSet {
    relations: Vec<Relation>,
    spanning: Option<String>,
    registry: Option<String>,
    config: Option<String>,
    deletions: Option<String>,
    recycle: Option<String>,
    history: Option<String>,
    about: Option<String>,
}

impl RelationSet {
    /// An empty relation set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a relation (builder-style).
    pub fn with(mut self, relation: Relation) -> Self {
        self.relations.push(relation);
        self
    }

    /// Drop the named relation, if present (builder-style) — after this, the
    /// field is not a link here, so [`edges`](Self::edges) ignores a document key
    /// by that name and the value reads as ordinary carried content.
    ///
    /// The counterpart to [`with`](Self::with), and what makes a preset an
    /// *overlay base* rather than an all-or-nothing choice: a config that starts
    /// from [`diaryx`](Self::diaryx) and declares one relation needs a way to
    /// both redefine a name (remove, then add) and retract one, without
    /// restating the vocabulary it was otherwise happy with. See
    /// `WorkspaceConfig::relation_set`.
    ///
    /// The **pointer marks** are deliberately untouched: dropping `registry`
    /// stops it being a relation but leaves `registry_relation()` answering,
    /// because that pointer is how a reader finds the workspace's machinery at
    /// all (§6) and is not the vocabulary's to revoke.
    pub fn without(mut self, name: &str) -> Self {
        self.relations.retain(|r| r.name != name);
        self
    }

    /// Mark the named relation as the spanning (canonical tree) relation.
    pub fn spanning(mut self, name: impl Into<String>) -> Self {
        self.spanning = Some(name.into());
        self
    }

    /// Mark the named relation as the **registry pointer**: the root document
    /// links its ID registry through this relation, which is what makes the
    /// registry *reachable* — workspace-critical state discovered by following
    /// links from the root, like everything else, rather than hidden in an
    /// app-private sidecar folder.
    pub fn registry(mut self, name: impl Into<String>) -> Self {
        self.registry = Some(name.into());
        self
    }

    /// Mark the named relation as the **config pointer**: the root document links
    /// its workspace-config document through this relation — the same
    /// reachability move as the registry (§6), so workspace policy
    /// (`link_format`, defaults, …) is a self-describing node discovered by
    /// following links from the root, never an app-private sidecar. The config
    /// document is optional and lazily created; its absence means all defaults.
    pub fn config(mut self, name: impl Into<String>) -> Self {
        self.config = Some(name.into());
        self
    }

    /// Mark the named relation as the **deletion-log pointer**: the root
    /// document links its deletion log through this relation — the same
    /// reachability move as the registry and config (§6). A delete destroys the
    /// bytes and records what it destroyed: where the document sat, what it was
    /// called, which id it held, and which parent listed it. That record is what
    /// [`restore`] repairs the graph from once the bytes are back. Making the
    /// log *reachable* is what keeps it honest: `check` validates it like any
    /// other member, and nothing about a deletion is hidden in an app-private
    /// folder.
    ///
    /// [`restore`]: https://docs.rs/prov/latest/prov/struct.Workspace.html#method.restore
    pub fn deletions(mut self, name: impl Into<String>) -> Self {
        self.deletions = Some(name.into());
        self
    }

    /// Mark the named relation as the **legacy recycle-bin pointer** — the
    /// spelling [`deletions`](Self::deletions) replaced.
    ///
    /// Kept only so a root written before the rename still resolves: the log is
    /// read through this pointer when the document declares no `deletions`, and
    /// `check` reports the old spelling as a rename to make. Nothing writes it.
    /// A workspace that parked bytes under this pointer's `items/` keeps them
    /// parked out of every walk for as long as it declares it.
    pub fn recycle(mut self, name: impl Into<String>) -> Self {
        self.recycle = Some(name.into());
        self
    }

    /// Mark the named relation as the **history pointer**: the root document links
    /// its history-store index through this relation — the same reachability move
    /// as the registry, config and deletion log (§6). The store holds one immutable
    /// event document per capture plus a content-addressed blob store, so a bad
    /// sync merge can be rolled back file by file. Making it *reachable* is what
    /// lets `check` validate it like any other member, and what keeps prov's own
    /// safety net out of an app-private folder.
    pub fn history(mut self, name: impl Into<String>) -> Self {
        self.history = Some(name.into());
        self
    }

    /// Mark the named relation as the **about pointer**: the root document links
    /// its generated `about.md` through this relation — structurally the same
    /// one-way move as the registry, config, deletion log and history (§6), but a
    /// distinct target kind (spec §4, *generated prose*), because the file is
    /// entirely prose in the workspace's content format rather than a whole-file
    /// record store.
    ///
    /// The pointer exists so *prov* can find the page to regenerate and validate
    /// it, and so the file is reachable rather than loose in the tree. It is
    /// deliberately **not** the human reader's way in: a person opening the
    /// directory finds `about.md` by its name, needing no pointer, no parser and
    /// no convention beyond being able to read a text file. That is the whole
    /// point of the artifact, and why the default filename is load-bearing.
    pub fn about(mut self, name: impl Into<String>) -> Self {
        self.about = Some(name.into());
        self
    }

    /// The diaryx vocabulary: `contents`/`part_of` containment (spanning),
    /// `links`/`link_of` arbitrary cross-references, `registry` (the root's
    /// pointer to its ID registry document), `config` (the root's pointer to its
    /// workspace-config document), `deletions` (the root's pointer to its
    /// deletion log), `history` (the root's pointer to its history store), and
    /// `about` (the root's pointer to its generated `about.md`).
    ///
    /// `recycle_bin` is here too, and is not one of those. It is the spelling
    /// `deletions` replaced, kept readable so a root written before the rename
    /// still resolves — see [`recycle`](Self::recycle).
    pub fn diaryx() -> Self {
        Self::new()
            .with(Relation::many("contents").inverse("part_of"))
            .with(Relation::one("part_of").inverse("contents"))
            .with(Relation::many("links").inverse("link_of"))
            .with(Relation::many("link_of").inverse("links"))
            .with(Relation::one("registry"))
            .with(Relation::one("config"))
            .with(Relation::one("deletions"))
            .with(Relation::one("recycle_bin"))
            .with(Relation::one("history"))
            .with(Relation::one("about"))
            .spanning("contents")
            .registry("registry")
            .config("config")
            .deletions("deletions")
            .recycle("recycle_bin")
            .history("history")
            .about("about")
    }

    /// prov's own human gloss for a [`diaryx`](Self::diaryx) **content**
    /// relation — what the preset would have written in a `means:` had the
    /// workspace bothered to declare it. `None` for any other name.
    ///
    /// The preset is the base every workspace's vocabulary overlays, so an
    /// undeclared `contents` is prov's `contents` and its meaning is known here
    /// rather than being a blank a reader has to guess at. Only the four content
    /// relations are glossed: the five pointers are machinery a consumer
    /// describes in its own words (see `prov`'s about page), not vocabulary a
    /// reader follows.
    pub fn diaryx_means(name: &str) -> Option<&'static str> {
        match name {
            "contents" => Some("documents contained by this one"),
            "part_of" => Some("the document that contains this one"),
            "links" => Some("arbitrary cross-references to other documents"),
            "link_of" => Some("documents that cross-reference this one"),
            _ => None,
        }
    }

    /// The configured relations.
    pub fn relations(&self) -> &[Relation] {
        &self.relations
    }

    /// The per-relation reference style override for `name`, if that relation is
    /// configured and carries one. `None` means "inherit the workspace default"
    /// — the caller falls back to its own default style.
    pub fn style_for(&self, name: &str) -> Option<ReferenceStyle> {
        self.relations
            .iter()
            .find(|r| r.name == name)
            .and_then(|r| r.style)
    }

    /// Overlay per-relation reference styles by name (builder-style) — the
    /// config-driven form of [`Relation::style`]. Each configured relation whose
    /// name appears in `styles` adopts that style; relations absent from the map
    /// keep whatever style they already carry (usually none → the workspace
    /// default). Names in `styles` with no matching relation are ignored. This is
    /// how a workspace's vocabulary picks up the `relations` block of its config
    /// document (see `prov`'s `WorkspaceConfig::resolved_relation_styles`).
    ///
    /// `prov`'s `WorkspaceConfig::resolved_relation_styles`: `prov`'s `WorkspaceConfig::resolved_relation_styles`
    pub fn with_styles(
        mut self,
        styles: &std::collections::BTreeMap<String, ReferenceStyle>,
    ) -> Self {
        for relation in &mut self.relations {
            if let Some(style) = styles.get(&relation.name) {
                relation.style = Some(*style);
            }
        }
        self
    }

    /// The name of the spanning relation, if one is configured.
    pub fn spanning_relation(&self) -> Option<&str> {
        self.spanning.as_deref()
    }

    /// The name of the registry-pointer relation, if one is configured.
    pub fn registry_relation(&self) -> Option<&str> {
        self.registry.as_deref()
    }

    /// The name of the config-pointer relation, if one is configured.
    pub fn config_relation(&self) -> Option<&str> {
        self.config.as_deref()
    }

    /// The name of the deletion-log-pointer relation, if one is configured.
    pub fn deletions_relation(&self) -> Option<&str> {
        self.deletions.as_deref()
    }

    /// The name of the **legacy** recycle-bin-pointer relation, if one is
    /// configured — the spelling [`deletions_relation`](Self::deletions_relation)
    /// replaced, resolved only when a root declares no `deletions` pointer.
    pub fn recycle_relation(&self) -> Option<&str> {
        self.recycle.as_deref()
    }

    /// The name of the history-pointer relation, if one is configured.
    pub fn history_relation(&self) -> Option<&str> {
        self.history.as_deref()
    }

    /// The name of the about-pointer relation, if one is configured.
    pub fn about_relation(&self) -> Option<&str> {
        self.about.as_deref()
    }

    /// Extract every link declared by a document's metadata, tagged by relation.
    pub fn edges(&self, meta: &fig::Value) -> Vec<Edge> {
        let mut edges = Vec::new();
        for relation in &self.relations {
            let Some(value) = meta.get(relation.name.as_str()) else {
                continue;
            };
            for target in crate::meta::link_strings(value) {
                edges.push(Edge {
                    relation: relation.name.clone(),
                    target,
                });
            }
        }
        edges
    }

    /// The raw targets of the spanning relation — i.e. this node's children in
    /// the canonical tree. Empty if no spanning relation is configured or the
    /// field is absent.
    pub fn children(&self, meta: &fig::Value) -> Vec<String> {
        match self.spanning.as_deref().and_then(|name| meta.get(name)) {
            Some(value) => crate::meta::link_strings(value),
            None => Vec::new(),
        }
    }
}

// These tests use YAML frontmatter fixtures, so they run under the `yaml` feature.
#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use crate::document::Document;

    fn doc(text: &str) -> Document {
        Document::parse("index.md", text).unwrap()
    }

    #[test]
    fn extracts_edges_tagged_by_relation() {
        let d = doc("---\ncontents:\n- a.md\n- b.md\npart_of: ../root.md\n---\nbody\n");
        let set = RelationSet::diaryx();
        let edges = set.edges(&fig::Value::from(&d.meta));
        assert_eq!(edges.len(), 3);
        assert!(edges.contains(&Edge {
            relation: "contents".into(),
            target: "a.md".into()
        }));
        assert!(edges.contains(&Edge {
            relation: "part_of".into(),
            target: "../root.md".into()
        }));
    }

    #[test]
    fn children_reads_the_spanning_relation() {
        let d = doc("---\ncontents:\n- a.md\n- b.md\n---\nbody\n");
        let set = RelationSet::diaryx();
        assert_eq!(
            set.children(&fig::Value::from(&d.meta)),
            vec!["a.md".to_string(), "b.md".to_string()]
        );
        assert_eq!(set.spanning_relation(), Some("contents"));
    }

    #[test]
    fn diaryx_declares_registry_config_deletions_history_and_about_pointers() {
        let set = RelationSet::diaryx();
        assert_eq!(set.registry_relation(), Some("registry"));
        assert_eq!(set.config_relation(), Some("config"));
        assert_eq!(set.deletions_relation(), Some("deletions"));
        assert_eq!(set.history_relation(), Some("history"));
        assert_eq!(set.about_relation(), Some("about"));
        // The spelling `deletions` replaced, still resolvable so a root written
        // before the rename keeps working.
        assert_eq!(set.recycle_relation(), Some("recycle_bin"));
        // Each is a single-valued pointer relation in the vocabulary.
        assert!(set.relations().iter().any(|r| r.name == "config"));
        assert!(set.relations().iter().any(|r| r.name == "deletions"));
        assert!(set.relations().iter().any(|r| r.name == "recycle_bin"));
        assert!(set.relations().iter().any(|r| r.name == "history"));
        assert!(set.relations().iter().any(|r| r.name == "about"));
        // `about` is one-way: it declares no inverse, so nothing writes a
        // back-link into the generated page (spec §4, generated prose).
        let about = set.relations().iter().find(|r| r.name == "about").unwrap();
        assert_eq!(about.inverse, None);
    }

    #[test]
    fn without_drops_the_relation_but_never_the_pointer_mark() {
        let d = doc("---\nlinks:\n- a.md\nregistry: registry.yaml\n---\nbody\n");
        let set = RelationSet::diaryx().without("links").without("registry");

        // Neither key is a link any more, so both read as ordinary carried
        // content — that is what retracting a relation means.
        assert!(set.edges(&fig::Value::from(&d.meta)).is_empty());
        assert!(!set.relations().iter().any(|r| r.name == "links"));
        // …but the registry is still findable, because the pointer is how a
        // reader reaches the workspace's machinery at all.
        assert_eq!(set.registry_relation(), Some("registry"));
        // Removing a name the set does not have is a no-op, not a panic.
        let untouched = RelationSet::diaryx().without("nonexistent");
        assert_eq!(untouched.relations().len(), 10);
    }

    #[test]
    fn diaryx_means_glosses_the_content_relations_only() {
        assert_eq!(
            RelationSet::diaryx_means("part_of"),
            Some("the document that contains this one")
        );
        // The pointers are machinery a consumer words for itself, and an
        // unknown name is not the preset's to describe.
        assert_eq!(RelationSet::diaryx_means("registry"), None);
        assert_eq!(RelationSet::diaryx_means("sections"), None);
        // Every glossed name is in fact a relation the preset declares.
        let set = RelationSet::diaryx();
        for name in ["contents", "part_of", "links", "link_of"] {
            assert!(RelationSet::diaryx_means(name).is_some(), "{name}");
            assert!(set.relations().iter().any(|r| r.name == name), "{name}");
        }
    }

    #[test]
    fn with_styles_attaches_config_styles_by_name() {
        use crate::link::{Addressing, LinkStyle, Wrapper};
        use std::collections::BTreeMap;

        let alias = ReferenceStyle {
            wrapper: Wrapper::Wikilink,
            addressing: Addressing::Alias,
            label: false,
            path_style: LinkStyle::default(),
        };
        let styles = BTreeMap::from([("contents".to_string(), alias)]);
        let set = RelationSet::diaryx().with_styles(&styles);

        // Named relation adopts the style; unnamed ones stay on the default.
        assert_eq!(set.style_for("contents"), Some(alias));
        assert_eq!(set.style_for("part_of"), None);
        // A name with no matching relation is ignored, not an error.
        let orphan = BTreeMap::from([("nonexistent".to_string(), alias)]);
        assert!(
            RelationSet::diaryx()
                .with_styles(&orphan)
                .style_for("contents")
                .is_none()
        );
    }

    #[test]
    fn custom_vocabulary_is_honored() {
        // Nothing diaryx-specific: organize by `part` / `whole`.
        let set = RelationSet::new()
            .with(Relation::many("part").inverse("whole"))
            .with(Relation::one("whole").inverse("part"))
            .spanning("part");
        let d = doc("---\npart:\n- one.md\n- two.md\n---\nbody\n");
        assert_eq!(
            set.children(&fig::Value::from(&d.meta)),
            vec!["one.md".to_string(), "two.md".to_string()]
        );
    }
}
