//! The medical preset, built into the binary, and a fresh workspace made with
//! it — what `prov init --preset presets/medical` does, through the library.
//!
//! Built in with `include_str!` from `presets/medical/`, so the importer and
//! the preset in the same commit cannot disagree; `--preset DIR` loads another
//! one from disk instead.

use std::path::{Path, PathBuf};

use prov::edit::{self, MetaEditor};
use prov::preset::Preset;
use prov::{
    AboutContext, Mapping, MetaFormat, StdFs, Value, Workspace, WorkspaceConfig, embed_carrier,
    meta,
};

use crate::Error;

const NODE: &str = include_str!("../../presets/medical/prov.yaml");
pub(crate) const CODE_SYSTEMS: &str = include_str!("../../presets/medical/vocab/code-systems.yaml");
pub(crate) const PROBLEM_STATUSES: &str =
    include_str!("../../presets/medical/vocab/problem-statuses.yaml");
pub(crate) const MEDICATION_STATUSES: &str =
    include_str!("../../presets/medical/vocab/medication-statuses.yaml");
const STORES: &[(&str, &str)] = &[
    (
        "vocab/acts.yaml",
        include_str!("../../presets/medical/vocab/acts.yaml"),
    ),
    (
        "vocab/audiences.yaml",
        include_str!("../../presets/medical/vocab/audiences.yaml"),
    ),
    ("vocab/code-systems.yaml", CODE_SYSTEMS),
    ("vocab/medication-statuses.yaml", MEDICATION_STATUSES),
    ("vocab/problem-statuses.yaml", PROBLEM_STATUSES),
    (
        "vocab/record-kinds.yaml",
        include_str!("../../presets/medical/vocab/record-kinds.yaml"),
    ),
];

/// The medical preset as shipped beside this crate.
pub fn medical() -> Result<Preset, Error> {
    let config = meta::parse_mapping(NODE, MetaFormat::Yaml)?;
    let issues = prov::diagnose(&Value::Mapping(config.clone()));
    if let Some(issue) = issues.first() {
        return Err(format!("the built-in medical preset does not read: {}", issue.key).into());
    }
    Ok(Preset {
        config,
        files: STORES
            .iter()
            .map(|(path, text)| (PathBuf::from(path), text.as_bytes().to_vec()))
            .collect(),
    })
}

/// The root document a fresh workspace gets.
pub const ROOT_DOC: &str = "index.md";
/// Its config document.
pub const CONFIG_DOC: &str = "prov.yaml";

/// Make a workspace in `dir` — which must be empty or not exist — titled
/// `title`, with `preset` applied: the root `index.md`, the config document
/// `prov.yaml` with prov's defaults, the preset's axes and stores, and
/// `about.md`. The same files `prov init --yes --preset DIR` writes.
pub async fn init(dir: &Path, title: &str, preset: &Preset) -> Result<(), Error> {
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        return Err(format!(
            "{}: not empty — refusing to make a workspace there",
            dir.display()
        )
        .into());
    }
    std::fs::create_dir_all(dir)?;

    let mut editor = MetaEditor::open_or_init(
        &format!("\n# {title}\n"),
        Some(embed_carrier(prov::EmbedStyle::Delimited, MetaFormat::Yaml)),
    )?;
    editor.set_value(
        &edit::key_path("title")?.segments(),
        edit::infer_scalar(title),
    )?;
    editor.set_value(
        &edit::key_path("config")?.segments(),
        edit::infer_scalar(CONFIG_DOC),
    )?;
    std::fs::write(dir.join(ROOT_DOC), editor.render()?)?;

    // prov's defaults are `init`'s: ids kept in frontmatter and a registry.
    let config = WorkspaceConfig::default();
    let mut node = Mapping::new();
    node.insert("title".into(), Value::String("prov config".into()));
    node.extend(config.to_mapping());
    std::fs::write(
        dir.join(CONFIG_DOC),
        meta::serialize_mapping(&node, MetaFormat::Yaml)?,
    )?;

    let root_doc = Path::new(ROOT_DOC);
    let mut probe: Workspace<StdFs> = Workspace::builder(StdFs).root(dir).build();
    probe.apply_preset(root_doc, preset).await?;
    let config = probe.effective_config(root_doc).await?;
    if prov::about::enabled(&config) {
        let probe: Workspace<StdFs> = Workspace::builder(StdFs)
            .root(dir)
            .relations(config.relation_set())
            .build();
        let about = AboutContext {
            root_doc: root_doc.to_path_buf(),
            config_doc: probe.config_path(root_doc).await?,
            registry_doc: probe.registry_path(root_doc).await?,
            deletions_doc: probe.deletions_path(root_doc).await?,
            history_doc: probe.history_path(root_doc).await?,
            reified: probe.reified_vocabularies(root_doc, &config).await?,
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        probe.write_about(root_doc, &config, &about).await?;
    }
    Ok(())
}
