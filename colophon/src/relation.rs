//! Relations — the configurable vocabulary of links declared in metadata.
//!
//! colophon is opinionated about the *mechanism* (links live in embedded
//! metadata; one relation is the canonical tree; the rest overlay it) but not
//! about the *vocabulary*. A [`RelationSet`] names which fields are links, their
//! cardinality, their inverse, and which single relation is **spanning**.

use crate::link::ReferenceStyle;
use crate::meta::Value;

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
    /// The reference style colophon authors *this* relation's links in,
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
    recycle: Option<String>,
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

    /// Mark the named relation as the **recycle-bin pointer**: the root document
    /// links its recycle-bin index through this relation — the same reachability
    /// move as the registry and config (§6). A deleted document is not destroyed
    /// but moved into the bin, and the bin's index (a self-describing member,
    /// discovered by following this link from the root) records where it came
    /// from so it can be restored. Making the bin *reachable* is what keeps it
    /// honest: `check` validates it like any other member, and nothing about a
    /// deletion is hidden in an app-private folder.
    pub fn recycle(mut self, name: impl Into<String>) -> Self {
        self.recycle = Some(name.into());
        self
    }

    /// The diaryx vocabulary: `contents`/`part_of` containment (spanning),
    /// `links`/`link_of` arbitrary cross-references, `registry` (the root's
    /// pointer to its ID registry document), `config` (the root's pointer to its
    /// workspace-config document), and `recycle_bin` (the root's pointer to its
    /// recycle-bin index).
    pub fn diaryx() -> Self {
        Self::new()
            .with(Relation::many("contents").inverse("part_of"))
            .with(Relation::one("part_of").inverse("contents"))
            .with(Relation::many("links").inverse("link_of"))
            .with(Relation::many("link_of").inverse("links"))
            .with(Relation::one("registry"))
            .with(Relation::one("config"))
            .with(Relation::one("recycle_bin"))
            .spanning("contents")
            .registry("registry")
            .config("config")
            .recycle("recycle_bin")
    }

    /// The configured relations.
    pub fn relations(&self) -> &[Relation] {
        &self.relations
    }

    /// The per-relation reference style override for `name`, if that relation is
    /// configured and carries one. `None` means "inherit the workspace default"
    /// — the caller falls back to its own default style.
    pub fn style_for(&self, name: &str) -> Option<ReferenceStyle> {
        self.relations.iter().find(|r| r.name == name).and_then(|r| r.style)
    }

    /// Overlay per-relation reference styles by name (builder-style) — the
    /// config-driven form of [`Relation::style`]. Each configured relation whose
    /// name appears in `styles` adopts that style; relations absent from the map
    /// keep whatever style they already carry (usually none → the workspace
    /// default). Names in `styles` with no matching relation are ignored. This is
    /// how a workspace's vocabulary picks up the `relations` block of its config
    /// document (see [`WorkspaceConfig::resolved_relation_styles`]).
    ///
    /// [`WorkspaceConfig::resolved_relation_styles`]: crate::config::WorkspaceConfig::resolved_relation_styles
    pub fn with_styles(mut self, styles: &std::collections::BTreeMap<String, ReferenceStyle>) -> Self {
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

    /// The name of the recycle-bin-pointer relation, if one is configured.
    pub fn recycle_relation(&self) -> Option<&str> {
        self.recycle.as_deref()
    }

    /// Extract every link declared by a document's metadata, tagged by relation.
    pub fn edges(&self, meta: &Value) -> Vec<Edge> {
        let mut edges = Vec::new();
        for relation in &self.relations {
            let Some(value) = meta.get(&relation.name) else {
                continue;
            };
            for target in value.link_strings() {
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
    pub fn children(&self, meta: &Value) -> Vec<String> {
        match self.spanning.as_deref().and_then(|name| meta.get(name)) {
            Some(value) => value.link_strings(),
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
        let edges = set.edges(&d.meta);
        assert_eq!(edges.len(), 3);
        assert!(edges.contains(&Edge { relation: "contents".into(), target: "a.md".into() }));
        assert!(edges.contains(&Edge { relation: "part_of".into(), target: "../root.md".into() }));
    }

    #[test]
    fn children_reads_the_spanning_relation() {
        let d = doc("---\ncontents:\n- a.md\n- b.md\n---\nbody\n");
        let set = RelationSet::diaryx();
        assert_eq!(set.children(&d.meta), vec!["a.md".to_string(), "b.md".to_string()]);
        assert_eq!(set.spanning_relation(), Some("contents"));
    }

    #[test]
    fn diaryx_declares_registry_config_and_recycle_pointers() {
        let set = RelationSet::diaryx();
        assert_eq!(set.registry_relation(), Some("registry"));
        assert_eq!(set.config_relation(), Some("config"));
        assert_eq!(set.recycle_relation(), Some("recycle_bin"));
        // Each is a single-valued pointer relation in the vocabulary.
        assert!(set.relations().iter().any(|r| r.name == "config"));
        assert!(set.relations().iter().any(|r| r.name == "recycle_bin"));
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
        assert!(RelationSet::diaryx().with_styles(&orphan).style_for("contents").is_none());
    }

    #[test]
    fn custom_vocabulary_is_honored() {
        // Nothing diaryx-specific: organize by `part` / `whole`.
        let set = RelationSet::new()
            .with(Relation::many("part").inverse("whole"))
            .with(Relation::one("whole").inverse("part"))
            .spanning("part");
        let d = doc("---\npart:\n- one.md\n- two.md\n---\nbody\n");
        assert_eq!(set.children(&d.meta), vec!["one.md".to_string(), "two.md".to_string()]);
    }
}
