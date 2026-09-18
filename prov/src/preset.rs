//! `preset` — a stencil of configuration, written out into a workspace.
//!
//! A preset is a bundle of what a common kind of workspace needs — a `status`
//! vocabulary, the views that answer "what is open", the fields a new document
//! starts with — that prov writes for you, in full, and then forgets. prov's
//! reader never learns a preset's name: once applied, the workspace is
//! ordinary, fully spelled-out config, and a reader in twenty years with no
//! prov binary can tell from the vocabulary file that `status: dropped` was one
//! of four closed terms. That is the whole argument of
//! `docs/proposals/presets/proposal-presets-v1.md`, and it is why this module
//! contains a writer and no reader.
//!
//! **On disk, a preset is a directory laid out like the root of the workspace
//! it will be merged into**: a config node (`prov.yaml`, or any whole-file
//! format) carrying only the axes the preset declares, and beside it, at the
//! paths that config's links name, the stores those axes point at — a
//! vocabulary file for each `fields` entry that has one. Because the layout is
//! the workspace's own, a root-relative link means the same thing in the
//! preset directory as it does after the copy.
//!
//! **prov ships exactly one preset, and it has no name.** [`Preset::builtin`]
//! is what `init` writes when told nothing else — the `created` and `updated`
//! axes and nothing more. It is a preset rather than a hard-coded default so
//! that there is one mechanism, and so that a preset directory has the power
//! to *replace* it: `init --preset <dir>` writes the directory and not the
//! built-in. The binary carries no list of preset names for a second entry to
//! be added to; every other preset is a directory, named by its path.
//!
//! **Applying is additive and refuses collisions.** Each config entry the
//! preset declares is written where the workspace does not declare it, or
//! declares it at its default; an entry already declared with the same value
//! is nothing to do; an entry declared *differently* is a collision, reported
//! and never resolved by guessing, because the two declarations mean different
//! things and prov cannot choose. Each store is written where nothing sits at
//! its path; a file already there with the same bytes is nothing to do, and one
//! with different bytes is a collision. [`Workspace::plan_preset`] says all of
//! that without writing, in a [`Plan`]; [`Workspace::apply_preset`] lands a
//! clean plan in one crash-safe change set and refuses one that is not.

use std::path::{Path, PathBuf};

use prov_graph::document::{Document, whole_file_format};
use prov_graph::error::{Error, Result};
use prov_graph::link;
use prov_graph::meta::{Mapping, Value};
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

use crate::config::{ROOT_CONFIG_KEY, WorkspaceConfig};
use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;

/// A preset, loaded: the config axes it declares and the stores it carries.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    /// The config the preset declares — the mapping of its node, top-level
    /// keys as the config document spells them. Only what the preset says;
    /// never a full config.
    pub config: Mapping,
    /// The stores beside the node, as workspace-relative paths and bytes, in
    /// directory order.
    pub files: Vec<(PathBuf, Vec<u8>)>,
}

impl Preset {
    /// The one preset prov ships, unnamed: what a workspace gets when told
    /// nothing else. The `created` and `updated` axes, each naming the field
    /// of the same name, so that every document made here records when, and
    /// every edit prov lands records that it did.
    ///
    /// Built in code rather than parsed from an embedded file so that it
    /// exists under every metadata-format feature set — a build without the
    /// `yaml` parser still has a default.
    pub fn builtin() -> Self {
        let mut config = Mapping::new();
        config.insert("created".into(), Value::String("created".into()));
        config.insert("updated".into(), Value::String("updated".into()));
        Self {
            config,
            files: Vec::new(),
        }
    }

    /// Load a preset from a directory: its node — the one file whose stem is
    /// `prov` in a whole-file format — and every other file under it, as the
    /// stores it carries at their preset-relative paths.
    ///
    /// The node is parsed and read through [`WorkspaceConfig`]'s own
    /// diagnosis, so a preset with a misspelled axis is refused here rather
    /// than written into a workspace where `check` would report it.
    pub fn load(dir: &Path) -> Result<Self> {
        let mut node = None;
        let mut files = Vec::new();
        collect(dir, dir, &mut node, &mut files)?;
        let Some((node_rel, text)) = node else {
            return Err(Error::Structure(format!(
                "{}: not a preset — no `prov.<yaml|json|toml|figl>` node in it",
                dir.display()
            )));
        };
        let doc = Document::parse(&node_rel, &text)?;
        let config =
            doc.meta.as_mapping().cloned().ok_or_else(|| {
                Error::Structure(format!("{}: not a mapping", node_rel.display()))
            })?;
        let issues = crate::config::diagnose(&doc.meta);
        if !issues.is_empty() {
            let mut lines = format!("{}: not a preset prov can read:", node_rel.display());
            for issue in issues {
                use crate::config::ConfigIssueKind as K;
                let what = match &issue.kind {
                    K::UnknownKey { suggestion } => {
                        format!("unknown key — did you mean `{suggestion}`?")
                    }
                    K::InvalidValue { value, expected } => {
                        format!(
                            "`{value}` is not a valid value (expected: {})",
                            expected.join(", ")
                        )
                    }
                    K::SpanningNotSingleParent { inverse } => {
                        format!("spanning relation's inverse `{inverse}` is not `cardinality: one`")
                    }
                    K::MalformedWorkspaceId { value } => {
                        format!("`{value}` is not a workspace name")
                    }
                    K::MalformedRoot { value } => format!("`{value}` is not a root name"),
                    K::NestNotSingleValued { field } => {
                        format!("nests by `{field}`, which is declared `type: seq`")
                    }
                    K::ScopedReference { field } => {
                        format!("scopes `{field}`, which is declared `type: ref`")
                    }
                };
                lines.push_str(&format!("\n  {}: {what}", issue.key));
            }
            return Err(Error::Structure(lines));
        }
        Ok(Self { config, files })
    }

    /// The preset without one declared axis — for a caller whose own
    /// explicit setting must win over the preset's, as an `init --updated-field`
    /// does over the built-in's `updated`.
    pub fn without(mut self, key: &str) -> Self {
        self.config.shift_remove(key);
        self
    }
}

/// Walk a preset directory, separating the node from the stores.
fn collect(
    root: &Path,
    dir: &Path,
    node: &mut Option<(PathBuf, String)>,
    files: &mut Vec<(PathBuf, Vec<u8>)>,
) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| Error::Structure(format!("{}: {e}", dir.display())))?
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| Error::Structure(format!("{}: {e}", dir.display())))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let rel = link::normalize(path.strip_prefix(root).unwrap_or(&path));
        if path.is_dir() {
            collect(root, &path, node, files)?;
            continue;
        }
        let is_node = dir == root
            && path.file_stem().and_then(|s| s.to_str()) == Some("prov")
            && whole_file_format(&path).is_some();
        if is_node {
            if let Some((other, _)) = node {
                return Err(Error::Structure(format!(
                    "{}: two nodes, {} and {} — a preset has one",
                    root.display(),
                    other.display(),
                    rel.display()
                )));
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| Error::Structure(format!("{}: {e}", path.display())))?;
            *node = Some((rel, text));
        } else {
            let bytes = std::fs::read(&path)
                .map_err(|e| Error::Structure(format!("{}: {e}", path.display())))?;
            files.push((rel, bytes));
        }
    }
    Ok(())
}

/// One thing applying a preset would do, or refuse to.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// A config entry the workspace does not declare, or declares at its
    /// default: written, at the dotted `key`.
    Set { key: String, value: Value },
    /// A config entry already declared with the same value: nothing to do.
    Same { key: String },
    /// A config entry declared with a different value: a collision. Nothing
    /// is written for it, and a plan carrying one is not applied.
    Differs {
        key: String,
        existing: Value,
        incoming: Value,
    },
    /// A store to write, at a path where nothing sits.
    Write { path: PathBuf },
    /// A store already present with identical bytes: nothing to do.
    Present { path: PathBuf },
    /// A store present with different bytes: a collision.
    Occupied { path: PathBuf },
}

impl Step {
    /// Whether this step is a collision — something the plan cannot do.
    pub fn is_collision(&self) -> bool {
        matches!(self, Step::Differs { .. } | Step::Occupied { .. })
    }

    /// Whether this step writes anything.
    pub fn writes(&self) -> bool {
        matches!(self, Step::Set { .. } | Step::Write { .. })
    }
}

/// What applying a preset to a workspace would do — every entry and store
/// judged, in the preset's order, config first.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// The steps, in the order they would be taken.
    pub steps: Vec<Step>,
    /// The document config entries are written into: the workspace's config
    /// document when the root names one, else the root itself (under its
    /// `prov:` block).
    pub surface: PathBuf,
    /// Whether `surface` is the root document — in which case each entry's
    /// key is written under `prov.`.
    pub in_root_block: bool,
}

impl Plan {
    /// Whether the plan can be applied — no step is a collision.
    pub fn is_clean(&self) -> bool {
        !self.steps.iter().any(Step::is_collision)
    }

    /// Whether applying would write anything at all.
    pub fn writes_anything(&self) -> bool {
        self.steps.iter().any(Step::writes)
    }

    /// The collisions, for reporting.
    pub fn collisions(&self) -> impl Iterator<Item = &Step> {
        self.steps.iter().filter(|s| s.is_collision())
    }
}

/// The top-level keys whose value is a mapping of *entries* — one declaration
/// per second-level key — and so merge one level down: two presets can each
/// declare a field, and a workspace that already declares `fields.audience`
/// has not thereby declared `fields.status`.
fn merges_by_entry(key: &str) -> bool {
    matches!(
        key,
        "fields" | "views" | "exports" | "relations" | "metadata" | "references"
    )
}

/// Look a dotted key of one or two segments up in a mapping.
fn lookup<'a>(map: &'a Mapping, key: &str) -> Option<&'a Value> {
    match key.split_once('.') {
        None => map.get(key),
        Some((head, tail)) => map.get(head).and_then(Value::as_mapping)?.get(tail),
    }
}

/// Recursively overlay `overlay` onto `base`: a mapping-valued key present in
/// both merges key by key; every other key is replaced. What the CLI does to
/// see the two config homes as one surface, done here for the same reason.
fn deep_merge(base: &mut Mapping, overlay: &Mapping) {
    for (key, value) in overlay {
        match (base.get_mut(key), value) {
            (Some(Value::Mapping(inner)), Value::Mapping(over)) => deep_merge(inner, over),
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// What applying `preset` to the workspace rooted at `root_doc` would do.
    /// Reads, never writes. See the module documentation for the rule each
    /// step follows.
    pub async fn plan_preset(&self, root_doc: &Path, preset: &Preset) -> Result<Plan> {
        let root_doc = link::normalize(root_doc);
        // The config as *declared* — the root's `prov:` block under the config
        // document, raw, as written. Compared raw rather than through
        // `WorkspaceConfig` so that a preset's entry is judged against what the
        // author wrote and not against a normalized re-spelling of it.
        let mut declared = Mapping::new();
        let (_, root) = self.load(&root_doc).await?;
        if let Some(block) = root.meta.get(ROOT_CONFIG_KEY).and_then(Value::as_mapping) {
            deep_merge(&mut declared, block);
        }
        let config_doc = self.config_path(&root_doc).await?;
        if let Some(doc_path) = &config_doc {
            let (_, doc) = self.load(doc_path).await?;
            if let Some(map) = doc.meta.as_mapping() {
                deep_merge(&mut declared, map);
            }
        }
        let defaults = WorkspaceConfig::default().to_mapping();

        let mut steps = Vec::new();
        let mut judge = |key: String, incoming: &Value| {
            let step = match lookup(&declared, &key) {
                Some(existing) if existing == incoming => Step::Same { key },
                // Declared, but at the default: the spelling `init` writes for
                // an axis nobody has decided about, which is not a decision
                // the preset would be overriding.
                Some(existing) if lookup(&defaults, &key) == Some(existing) => Step::Set {
                    key,
                    value: incoming.clone(),
                },
                Some(existing) => Step::Differs {
                    key,
                    existing: existing.clone(),
                    incoming: incoming.clone(),
                },
                None => Step::Set {
                    key,
                    value: incoming.clone(),
                },
            };
            steps.push(step);
        };
        for (key, value) in &preset.config {
            match value.as_mapping() {
                Some(entries) if merges_by_entry(key) => {
                    for (name, entry) in entries {
                        judge(format!("{key}.{name}"), entry);
                    }
                }
                _ => judge(key.clone(), value),
            }
        }

        for (path, bytes) in &preset.files {
            let path = link::normalize(path);
            let step = if self.exists(&path).await? {
                if self.read_bytes(&path).await? == *bytes {
                    Step::Present { path }
                } else {
                    Step::Occupied { path }
                }
            } else {
                Step::Write { path }
            };
            steps.push(step);
        }

        let (surface, in_root_block) = match config_doc {
            Some(doc) => (doc, false),
            None => (root_doc, true),
        };
        Ok(Plan {
            steps,
            surface,
            in_root_block,
        })
    }

    /// Apply `preset` to the workspace rooted at `root_doc`: plan it, refuse
    /// the plan if any step is a collision, and otherwise land every write in
    /// one crash-safe change set — the config entries into the plan's
    /// surface, comment- and format-preservingly, and each store at its
    /// path. Returns the plan that was applied.
    ///
    /// A plan with nothing to write is applied trivially: a preset a workspace
    /// already carries in full is not an error, which is what lets a fixture
    /// assert that a repository's own config *is* the preset by applying it
    /// again.
    pub async fn apply_preset(&mut self, root_doc: &Path, preset: &Preset) -> Result<Plan> {
        let plan = self.plan_preset(root_doc, preset).await?;
        if !plan.is_clean() {
            let mut lines = String::from("the preset collides with what the workspace declares:");
            for step in plan.collisions() {
                match step {
                    Step::Differs { key, .. } => {
                        lines.push_str(&format!("\n  {key} is already declared, differently"));
                    }
                    Step::Occupied { path } => {
                        lines.push_str(&format!(
                            "\n  {} exists with different contents",
                            path.display()
                        ));
                    }
                    _ => {}
                }
            }
            return Err(Error::Structure(lines));
        }
        if !plan.writes_anything() {
            return Ok(plan);
        }

        let mut cs = self.change();
        let sets: Vec<(&String, &Value)> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Set { key, value } => Some((key, value)),
                _ => None,
            })
            .collect();
        if !sets.is_empty() {
            let (mut text, doc) = self.load(&plan.surface).await?;
            for (key, value) in sets {
                let dotted = if plan.in_root_block {
                    format!("{ROOT_CONFIG_KEY}.{key}")
                } else {
                    key.clone()
                };
                text = prov_store::edit::set_meta_in_text(&text, doc.carrier, &dotted, value)?;
            }
            cs.write(&plan.surface, text);
        }
        for step in &plan.steps {
            if let Step::Write { path } = step {
                let bytes = preset
                    .files
                    .iter()
                    .find(|(p, _)| link::normalize(p) == *path)
                    .map(|(_, b)| b.clone())
                    .unwrap_or_default();
                cs.expect_absent(path);
                cs.write(path, bytes);
            }
        }
        self.commit(cs).await?;
        Ok(plan)
    }
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_testkit::{read, write};

    fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("preset", tag)
    }

    fn ws(dir: &Path) -> Workspace<StdFs> {
        Workspace::builder(StdFs).root(dir).build()
    }

    fn tasks_preset() -> Preset {
        let mut status = Mapping::new();
        status.insert("values".into(), Value::String("closed".into()));
        status.insert(
            "vocabulary".into(),
            Value::String("[Statuses](/vocab/statuses.yaml)".into()),
        );
        status.insert("default".into(), Value::String("open".into()));
        let mut fields = Mapping::new();
        fields.insert("status".into(), Value::Mapping(status));
        let mut config = Mapping::new();
        config.insert("fields".into(), Value::Mapping(fields));
        config.insert("updated".into(), Value::String("updated".into()));
        Preset {
            config,
            files: vec![(
                PathBuf::from("vocab/statuses.yaml"),
                b"title: Statuses\nvocabulary:\n  field: status\n  values: closed\nterms:\n  open: {}\n  done: {}\n".to_vec(),
            )],
        }
    }

    #[test]
    fn a_fresh_workspace_takes_every_entry_and_store() {
        let dir = tempdir("preset-fresh");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nconfig: prov.yaml\n---\n",
        );
        write(&dir, "prov.yaml", "title: prov config\nupdated: ''\n");
        let mut w = ws(&dir);
        let plan = block_on(w.apply_preset(Path::new("index.md"), &tasks_preset())).unwrap();
        assert!(plan.is_clean());
        assert_eq!(plan.surface, PathBuf::from("prov.yaml"));
        assert!(!plan.in_root_block);
        // `updated: ''` is the default spelled out, so it is free to take.
        assert!(matches!(&plan.steps[0], Step::Set { key, .. } if key == "fields.status"));
        assert!(matches!(&plan.steps[1], Step::Set { key, .. } if key == "updated"));
        assert!(
            matches!(&plan.steps[2], Step::Write { path } if path == Path::new("vocab/statuses.yaml"))
        );

        let node = read(&dir, "prov.yaml");
        assert!(node.contains("updated: updated"), "{node}");
        assert!(
            node.contains("status:") && node.contains("default: open"),
            "{node}"
        );
        assert!(
            node.starts_with("title: prov config\n"),
            "format-preserving: {node}"
        );
        assert!(read(&dir, "vocab/statuses.yaml").contains("open: {}"));
        // And the workspace reads it back as its own config.
        let config = block_on(w.effective_config(Path::new("index.md"))).unwrap();
        assert_eq!(config.updated, "updated");
        assert_eq!(
            config.fields["status"][0].default,
            Some(Value::String("open".into()))
        );
    }

    #[test]
    fn applying_again_finds_nothing_to_do_and_a_change_is_a_collision() {
        let dir = tempdir("preset-again");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nconfig: prov.yaml\n---\n",
        );
        write(&dir, "prov.yaml", "title: prov config\n");
        let mut w = ws(&dir);
        block_on(w.apply_preset(Path::new("index.md"), &tasks_preset())).unwrap();

        let again = block_on(w.plan_preset(Path::new("index.md"), &tasks_preset())).unwrap();
        assert!(again.is_clean() && !again.writes_anything(), "{again:?}");
        assert!(
            again
                .steps
                .iter()
                .all(|s| matches!(s, Step::Same { .. } | Step::Present { .. }))
        );
        // Applying a plan with nothing to write is not an error.
        block_on(w.apply_preset(Path::new("index.md"), &tasks_preset())).unwrap();

        // The workspace changes its mind about one term and one field; the
        // preset now disagrees with it on both, and is refused whole.
        write(
            &dir,
            "vocab/statuses.yaml",
            "title: Statuses\nterms:\n  open: {}\n",
        );
        let mut preset = tasks_preset();
        preset
            .config
            .insert("updated".into(), Value::String("modified".into()));
        let plan = block_on(w.plan_preset(Path::new("index.md"), &preset)).unwrap();
        assert!(!plan.is_clean());
        assert_eq!(plan.collisions().count(), 2, "{plan:?}");
        assert!(matches!(&plan.steps[1], Step::Differs { key, .. } if key == "updated"));
        assert!(matches!(&plan.steps[2], Step::Occupied { .. }));
        let err = block_on(w.apply_preset(Path::new("index.md"), &preset)).unwrap_err();
        assert!(
            err.to_string().contains("updated is already declared"),
            "{err}"
        );
        assert!(
            err.to_string().contains("vocab/statuses.yaml exists"),
            "{err}"
        );
        // Nothing moved: the vocabulary is still the workspace's own edit.
        assert_eq!(
            read(&dir, "vocab/statuses.yaml"),
            "title: Statuses\nterms:\n  open: {}\n"
        );
    }

    #[test]
    fn a_workspace_without_a_config_document_takes_the_root_block() {
        let dir = tempdir("preset-root-block");
        write(
            &dir,
            "index.md",
            "---\ntitle: Root\nprov:\n  fixity: off\n---\nbody\n",
        );
        let mut w = ws(&dir);
        let plan = block_on(w.apply_preset(Path::new("index.md"), &Preset::builtin())).unwrap();
        assert!(plan.in_root_block);
        assert_eq!(plan.surface, PathBuf::from("index.md"));
        let root = read(&dir, "index.md");
        assert!(root.contains("  fixity: off\n"), "kept: {root}");
        assert!(
            root.contains("  created: created\n") && root.contains("  updated: updated\n"),
            "{root}"
        );
        assert!(root.ends_with("body\n"));
        let config = block_on(w.effective_config(Path::new("index.md"))).unwrap();
        assert_eq!(
            (config.created.as_str(), config.updated.as_str()),
            ("created", "updated")
        );
    }

    #[test]
    fn a_preset_directory_loads_its_node_and_stores_and_refuses_a_typo() {
        let dir = tempdir("preset-load");
        write(&dir, "prov.yaml", "fields:\n  status:\n    default: open\n");
        write(&dir, "vocab/statuses.yaml", "title: Statuses\n");
        write(&dir, "vocab/deeper/terms.yaml", "title: T\n");
        let preset = Preset::load(&dir).unwrap();
        assert_eq!(
            preset
                .files
                .iter()
                .map(|(p, _)| p.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("vocab/deeper/terms.yaml"),
                PathBuf::from("vocab/statuses.yaml")
            ]
        );
        assert!(preset.config.contains_key("fields"));

        write(&dir, "prov.yaml", "feilds:\n  status:\n    default: open\n");
        let err = Preset::load(&dir).unwrap_err().to_string();
        assert!(err.contains("feilds"), "{err}");

        let empty = tempdir("preset-empty");
        let err = Preset::load(&empty).unwrap_err().to_string();
        assert!(err.contains("no `prov."), "{err}");
    }
}
