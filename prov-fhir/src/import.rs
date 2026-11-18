//! Writing a [`Plan`] into a workspace, through the library.
//!
//! The order is what makes it safe to run twice:
//!
//! 1. **The import.** The patient's share of the export is written under
//!    `Imports` and covered by a manifest node (`kind: import`) — unless a
//!    node already records the same digest, when that one is used and nothing
//!    is written.
//! 2. **Where everything goes.** Each document already in the workspace is
//!    found by the `fhir:` it carries; a new one gets a parent — its section's
//!    index, or the year index its filing entry routes it to — and a free file
//!    name, and is created with its fields.
//! 3. **Bodies, and updates.** Once every document has a path, each body is
//!    rendered with its links and written. A document that was already there
//!    is rewritten only if what the importer owns of it changed: the fields
//!    in [`OWNED`], and the body. Its `audience` is the patient's, `generated`
//!    is history, and neither is touched — except that an `audience` the
//!    importer set (`emergency` for an active problem) follows the status it
//!    was set for, when the patient has not changed it since.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use prov::edit;
use prov::link::{format_reference, slug};
use prov::{
    ChangeSet, Discovery, FileIndex, Layout, Mapping, Minter, Now, Stamp, StdFs, Value, Workspace,
    WorkspaceConfig,
};

use crate::Error;
use crate::fhir::Record;
use crate::map::{self, Doc, Plan, Section, is_emergency};
use crate::vocab::Vocab;

/// The actor every imported document names as its `generated.by`.
pub const ACTOR: &str = "process:prov-fhir";

/// The fields the importer owns on a document it made: rewritten when the
/// export says something else, removed when the export no longer says them.
pub const OWNED: &[&str] = &[
    "kind",
    "date_of_service",
    "status",
    "clinician",
    "codes",
    "other_codes",
    "fhir",
];

/// How an import runs.
#[derive(Debug, Clone)]
pub struct Options {
    /// Put active disorders, current medications and active allergies in the
    /// `emergency` audience. Everything else starts as `self`.
    pub emergency: bool,
    /// The instant written into `generated.at`, RFC 3339.
    pub now: String,
}

/// What an import did.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The import's manifest node.
    pub import: PathBuf,
    /// Whether that node was already there — the same export imported before.
    pub import_reused: bool,
    /// Indexes made under the root.
    pub indexes: Vec<String>,
    /// Whether the root was retitled for the patient.
    pub retitled: bool,
    pub created: BTreeMap<Section, usize>,
    pub updated: BTreeMap<Section, usize>,
    pub unchanged: BTreeMap<Section, usize>,
    /// Resources not made into documents, by why.
    pub skipped: BTreeMap<String, usize>,
    /// Codings left out of `codes`, by system URI.
    pub unknown_systems: BTreeMap<String, usize>,
    /// Observations summarized into encounter tables.
    pub summarized: usize,
    /// Resources of types no document is made from, by type: in the import
    /// only.
    pub kept: BTreeMap<String, usize>,
    pub seconds: f64,
}

impl Report {
    pub fn total(map: &BTreeMap<Section, usize>) -> usize {
        map.values().sum()
    }
}

type Ws = Workspace<StdFs, Minter, FileIndex>;

/// Import `record` into the workspace rooted at `root_dir`, which must
/// already carry the medical preset's fields.
pub async fn import(root_dir: &Path, record: &Record, opts: &Options) -> Result<Report, Error> {
    let started = Instant::now();
    let mut found = match prov::discover(&StdFs, root_dir).await? {
        Discovery::Found(d) => d,
        Discovery::Ambiguous { candidates, .. } => {
            return Err(format!(
                "{}: ambiguous workspace root ({})",
                root_dir.display(),
                candidates.join(", ")
            )
            .into());
        }
        Discovery::NotFound => {
            return Err(format!(
                "{}: no prov workspace here — make one with `prov init --preset presets/medical`, \
                 or use `prov-fhir import --each`",
                root_dir.display()
            )
            .into());
        }
    };
    let config = found.config.clone();
    if !config.fields.contains_key("kind") || !config.fields.contains_key("codes[].system") {
        return Err(format!(
            "{}: this workspace does not declare the medical preset's fields (`kind`, \
             `codes[].system`) — apply it first: `prov presets presets/medical --write`",
            root_dir.display()
        )
        .into());
    }
    if config.mints_on_mutation()
        && let Some(boot) = prov::ensure_registry(StdFs, found.as_root()).await?
    {
        found.registry = Some(boot.path);
    }
    let root_doc = found.root_doc.clone();
    let mut ws: Ws = Workspace::open_for_writing(StdFs, found.as_root(), seed()).await?;
    let vocab = Vocab::of_workspace(&ws, &root_doc, &config).await?;
    let plan = map::plan(record, &vocab);

    let mut importer = Importer {
        ws: &mut ws,
        config: &config,
        root_doc: root_doc.clone(),
        now: Now::utc(opts.now.clone()),
        opts,
        report: Report {
            skipped: plan.skipped.clone(),
            unknown_systems: plan.unknown_systems.clone(),
            summarized: plan.summarized,
            kept: record.kept.clone(),
            ..Report::default()
        },
        sections: HashMap::new(),
        routes: HashMap::new(),
        used: BTreeSet::new(),
    };
    importer.patient(&plan).await?;
    importer.sections(&plan).await?;
    let existing = importer.existing().await?;
    let (import, import_title) = importer.import_node(record, &existing).await?;
    importer
        .documents(&plan, &existing.docs, &import, &import_title)
        .await?;
    let mut report = importer.report;
    ws.persist_identity(found.as_root()).await?;
    report.import = import;
    report.seconds = started.elapsed().as_secs_f64();
    Ok(report)
}

/// What the workspace already holds of earlier imports.
struct Existing {
    /// Documents by key: `Encounter/<id>`, `Medication|<system>|<code>`, …
    docs: HashMap<String, PathBuf>,
    /// Import nodes by the digest of the export they keep.
    imports: HashMap<String, (PathBuf, String)>,
}

struct Importer<'w> {
    ws: &'w mut Ws,
    config: &'w WorkspaceConfig,
    root_doc: PathBuf,
    now: Now,
    opts: &'w Options,
    report: Report,
    /// Each section's index.
    sections: HashMap<Section, PathBuf>,
    /// The container each filing route ended in.
    routes: HashMap<String, PathBuf>,
    /// Paths given out in this run.
    used: BTreeSet<PathBuf>,
}

impl Importer<'_> {
    /// The `updated` stamp an edit writes, when the workspace keeps one.
    fn edit_stamp(&self) -> Option<(String, String)> {
        self.config
            .stamp_value(Stamp::Edit, &self.now)
            .map(|(f, v)| (f.to_string(), v))
    }

    async fn save(&mut self, path: &Path, text: &str) -> Result<(), Error> {
        let stamp = self.edit_stamp();
        self.ws
            .save_document(
                path,
                text,
                stamp.as_ref().map(|(f, v)| (f.as_str(), v.as_str())),
            )
            .await?;
        Ok(())
    }

    /// The fields a new document opens with besides its own: a `created`
    /// stamp, when the workspace keeps one.
    fn opening(&self) -> Mapping {
        let mut m = Mapping::new();
        if let Some((field, value)) = self.config.stamp_value(Stamp::Create, &self.now) {
            m.insert(field.to_string(), Value::String(value));
        }
        m
    }

    /// The root is the patient: titled with their name on the first import,
    /// and carrying who they are. A workspace that already holds someone
    /// else's record is refused.
    async fn patient(&mut self, plan: &Plan) -> Result<(), Error> {
        let p = &plan.patient;
        let (_, root) = self.ws.graph().load(&self.root_doc).await?;
        let holds = root
            .meta
            .get("fhir")
            .and_then(|f| f.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        match holds.as_deref() {
            Some(id) if id != p.id => {
                return Err(format!(
                    "this workspace keeps the record of patient {id}; {} ({}) is someone else — \
                     one workspace holds one patient's record",
                    p.id, p.name
                )
                .into());
            }
            Some(_) => {}
            None => {
                let title = root.meta.get("title").and_then(Value::as_str);
                if title != Some(p.name.as_str()) {
                    self.ws.retitle(&self.root_doc, &p.name).await?;
                    self.report.retitled = true;
                }
            }
        }
        let mut fields = Mapping::new();
        let mut fhir = Mapping::new();
        fhir.insert("resourceType".into(), Value::String("Patient".into()));
        fhir.insert("id".into(), Value::String(p.id.clone()));
        fields.insert("fhir".into(), Value::Mapping(fhir));
        for (key, value) in [
            ("birth_date", &p.birth_date),
            ("gender", &p.gender),
            ("deceased", &p.deceased),
        ] {
            if let Some(v) = value {
                fields.insert(key.into(), Value::String(v.clone()));
            }
        }
        let root_doc = self.root_doc.clone();
        let (text, root) = self.ws.graph().load(&root_doc).await?;
        let mut new = text.clone();
        for (key, value) in &fields {
            if root.meta.get(key) != Some(value) {
                new = edit::set_meta_in_text(&new, root.carrier, key, value)?;
            }
        }
        if new != text {
            self.save(&root_doc, &new).await?;
        }
        Ok(())
    }

    /// Find each section's index under the root, making the ones the import
    /// needs: the six the medical preset names, and Allergies, Immunizations
    /// and Procedures when the record has any. A new index is a folder note
    /// (`encounters/index.md`), so the year indexes filing makes below
    /// Encounters and below Labs land in different directories.
    async fn sections(&mut self, plan: &Plan) -> Result<(), Error> {
        let present: BTreeSet<Section> = plan.docs.iter().map(|d| d.section).collect();
        let defaults = self
            .ws
            .defaults_for_child(&self.root_doc, self.config, &self.root_doc)
            .await?;
        for &section in Section::ALL {
            let required = matches!(
                section,
                Section::Encounters
                    | Section::Problems
                    | Section::Medications
                    | Section::Labs
                    | Section::CareTeam
                    | Section::Imports
            );
            let route = self
                .ws
                .plan_route(&self.root_doc, &[section.title()], Layout::Nested)
                .await?;
            if route.is_complete() {
                self.sections.insert(section, route.terminal);
                continue;
            }
            if !required && !present.contains(&section) {
                continue;
            }
            let ext = self.config.content_format.extension();
            let path = PathBuf::from(slug(section.title())).join(format!("index.{ext}"));
            let mut fields = self.opening();
            fields.extend(defaults.clone());
            self.ws
                .create_with_fields(&path, &self.root_doc, section.title(), &fields)
                .await?;
            self.report.indexes.push(section.title().to_string());
            self.sections.insert(section, path);
        }
        Ok(())
    }

    /// Every document an earlier import made, by key, and every import node.
    async fn existing(&mut self) -> Result<Existing, Error> {
        let mut docs = HashMap::new();
        let mut imports = HashMap::new();
        for path in self.ws.reachable_documents_from(&self.root_doc).await? {
            let (_, doc) = self.ws.graph().load(&path).await?;
            let Some(fhir) = doc.meta.get("fhir") else {
                continue;
            };
            let s = |k: &str| fhir.get(k).and_then(Value::as_str);
            if let Some(export) = s("export") {
                let title = doc
                    .meta
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("FHIR export")
                    .to_string();
                imports.insert(export.to_string(), (path, title));
            } else if let Some(m) = s("medication") {
                docs.insert(format!("Medication|{m}"), path);
            } else if path != self.root_doc
                && let (Some(t), Some(id)) = (s("resourceType"), s("id"))
            {
                docs.insert(format!("{t}/{id}"), path);
            }
        }
        Ok(Existing { docs, imports })
    }

    /// The import's manifest node: the one already keeping this export, or
    /// a new one over the patient's share of it, written beside the Imports
    /// index.
    async fn import_node(
        &mut self,
        record: &Record,
        existing: &Existing,
    ) -> Result<(PathBuf, String), Error> {
        let digest = record.digest();
        if let Some((node, title)) = existing.imports.get(&digest) {
            self.report.import_reused = true;
            return Ok((node.clone(), title.clone()));
        }
        let index = self.sections[&Section::Imports].clone();
        let day = &self.opts.now[..10.min(self.opts.now.len())];
        let short = digest.trim_start_matches("sha256:");
        let dir = index
            .parent()
            .unwrap_or(Path::new(""))
            .join(format!("{day}-{}", &short[..8.min(short.len())]));
        let mut cs = ChangeSet::new();
        for (name, bytes) in &record.raw {
            cs.write(dir.join(name), bytes.clone());
        }
        self.ws.apply_set(&cs).await?;
        let title = format!("FHIR export, {day}");
        // Verbatim: the export is one artifact, and a Bundle's `.json` is a
        // file prov could read as a document — it is evidence, not a page.
        let node = self
            .ws
            .attach_manifest_claiming(&dir, &index, Some(&title), true, true)
            .await?;

        let mut fields = Mapping::new();
        fields.insert("kind".into(), Value::String("import".into()));
        let mut fhir = Mapping::new();
        fhir.insert("export".into(), Value::String(digest));
        fhir.insert("patient".into(), Value::String(record.patient_id().into()));
        fields.insert("fhir".into(), Value::Mapping(fhir));
        fields.insert("generated".into(), self.generated());
        fields.extend(
            self.ws
                .defaults_for_child(&self.root_doc, self.config, &index)
                .await?,
        );
        let (text, doc) = self.ws.graph().load(&node).await?;
        let mut new = text.clone();
        for (key, value) in &fields {
            new = edit::set_meta_in_text(&new, doc.carrier, key, value)?;
        }
        self.save(&node, &new).await?;
        Ok((node, title))
    }

    fn generated(&self) -> Value {
        let mut g = Mapping::new();
        g.insert("by".into(), Value::String(ACTOR.into()));
        g.insert("at".into(), Value::String(self.opts.now.clone()));
        g.insert("how".into(), Value::String("imported".into()));
        Value::Mapping(g)
    }

    /// The container a new document is made in: its section's index, or the
    /// index its filing entry routes its `date_of_service` to.
    async fn parent_for(&mut self, doc: &Doc) -> Result<PathBuf, Error> {
        let index = self.sections[&doc.section].clone();
        let Some(name) = doc.section.filing() else {
            return Ok(index);
        };
        let Some(spec) = self.config.filing.iter().find(|f| f.name == name) else {
            return Ok(index);
        };
        let mut meta = Mapping::new();
        if let Some(date) = doc.date_of_service() {
            meta.insert("date_of_service".into(), Value::String(date.into()));
        }
        let meta = Value::Mapping(meta);
        let route = spec.route(&meta).map(|r| format!("{name}:{r:?}"));
        if let Some(route) = &route
            && let Some(parent) = self.routes.get(route)
        {
            return Ok(parent.clone());
        }
        let opening = self.opening();
        let filed = self.ws.file(&self.root_doc, spec, &meta, &opening).await?;
        if let Some(route) = route {
            self.routes.insert(route, filed.parent.clone());
        }
        Ok(filed.parent)
    }

    /// A file name for `stem` beside `parent` that nothing holds.
    async fn free_path(&mut self, parent: &Path, stem: &str) -> Result<PathBuf, Error> {
        let dir = parent.parent().unwrap_or(Path::new(""));
        let ext = self.config.content_format.extension();
        for n in 1.. {
            let name = if n == 1 {
                format!("{stem}.{ext}")
            } else {
                format!("{stem}-{n}.{ext}")
            };
            let path = dir.join(name);
            if !self.used.contains(&path) && !self.ws.fs_path(&path).exists() {
                self.used.insert(path.clone());
                return Ok(path);
            }
        }
        unreachable!("an unbounded range ends only by returning")
    }

    /// A link from `from` to `to`, as this workspace writes one in a field.
    fn reference(&self, field: &str, from: &Path, to: &Path, title: &str) -> String {
        format_reference(self.ws.reference_style_for(field), from, to, None, title)
    }

    /// The fields the importer owns on `doc`, written at `path`.
    fn owned(
        &self,
        doc: &Doc,
        path: &Path,
        paths: &HashMap<String, PathBuf>,
        titles: &HashMap<String, String>,
    ) -> Mapping {
        let mut out = Mapping::new();
        for key in OWNED {
            if *key == "clinician" {
                if let Some(c) = &doc.clinician
                    && let Some(to) = paths.get(c)
                {
                    let title = titles.get(c).map(String::as_str).unwrap_or_default();
                    out.insert(
                        "clinician".into(),
                        Value::String(self.reference("clinician", path, to, title)),
                    );
                }
            } else if let Some(v) = doc.fields.get(*key) {
                out.insert((*key).to_string(), v.clone());
            }
        }
        out
    }

    fn audience(&self, fields: &Mapping) -> &'static str {
        if self.opts.emergency && is_emergency(&Value::Mapping(fields.clone())) {
            "emergency"
        } else {
            "self"
        }
    }

    async fn documents(
        &mut self,
        plan: &Plan,
        existing: &HashMap<String, PathBuf>,
        import: &Path,
        import_title: &str,
    ) -> Result<(), Error> {
        let mut paths: HashMap<String, PathBuf> = HashMap::new();
        let titles: HashMap<String, String> = plan
            .docs
            .iter()
            .map(|d| (d.key.clone(), d.title.clone()))
            .collect();
        let mut new: BTreeSet<String> = BTreeSet::new();

        // Where everything goes; the new ones made with their fields. The
        // care team comes first in the plan, so a clinician link always has
        // somewhere to point.
        for doc in &plan.docs {
            if let Some(path) = existing.get(&doc.key) {
                paths.insert(doc.key.clone(), path.clone());
                continue;
            }
            let parent = self.parent_for(doc).await?;
            let path = self.free_path(&parent, &doc.stem).await?;
            let mut fields = self.opening();
            let owned = self.owned(doc, &path, &paths, &titles);
            fields.extend(owned.clone());
            fields.insert(
                "audience".into(),
                Value::String(self.audience(&owned).into()),
            );
            fields.insert(
                "derived_from".into(),
                Value::String(self.reference("derived_from", &path, import, import_title)),
            );
            fields.insert("generated".into(), self.generated());
            self.ws
                .create_with_fields(&path, &parent, &doc.title, &fields)
                .await?;
            paths.insert(doc.key.clone(), path);
            new.insert(doc.key.clone());
        }

        // Bodies, now every link has a target; and what changed on the
        // documents that were already there.
        let link_style = self.ws.link_style();
        for doc in &plan.docs {
            let path = paths[&doc.key].clone();
            let is_new = new.contains(&doc.key);
            if !is_new {
                let (_, current) = self.ws.graph().load(&path).await?;
                if current.meta.get("title").and_then(Value::as_str) != Some(doc.title.as_str()) {
                    self.ws.retitle(&path, &doc.title).await?;
                }
            }
            let (text, current) = self.ws.graph().load(&path).await?;
            let body = doc.body.render(|key, label| {
                paths
                    .get(key)
                    .map(|to| prov::format_link(link_style, &path, to, label))
            });
            let Some(head) = text.strip_suffix(current.body.as_str()) else {
                return Err(
                    format!("{}: cannot find where its body starts", path.display()).into(),
                );
            };
            let mut next = format!("{head}{body}");
            let owned = self.owned(doc, &path, &paths, &titles);
            for (key, value) in &owned {
                if current.meta.get(key) != Some(value) {
                    next = edit::set_meta_in_text(&next, current.carrier, key, value)?;
                }
            }
            for key in OWNED {
                if !owned.contains_key(*key) && current.meta.get(key).is_some() {
                    next = edit::unset_in_text(&next, current.carrier, key)?;
                }
            }
            if next == text {
                *self.report.unchanged.entry(doc.section).or_default() += 1;
                continue;
            }
            if !is_new {
                // The new version came from this export.
                let from = self.reference("derived_from", &path, import, import_title);
                if current.meta.get("derived_from").and_then(Value::as_str) != Some(from.as_str()) {
                    next = edit::set_meta_in_text(
                        &next,
                        current.carrier,
                        "derived_from",
                        &Value::String(from),
                    )?;
                }
                // An audience the importer chose follows what it was chosen for.
                let was = self.audience(current.meta.as_mapping().unwrap_or(&Mapping::new()));
                let now = self.audience(&owned);
                if was != now && current.meta.get("audience").and_then(Value::as_str) == Some(was) {
                    next = edit::set_meta_in_text(
                        &next,
                        current.carrier,
                        "audience",
                        &Value::String(now.into()),
                    )?;
                }
            }
            self.save(&path, &next).await?;
            let tally = if is_new {
                &mut self.report.created
            } else {
                &mut self.report.updated
            };
            *tally.entry(doc.section).or_default() += 1;
        }
        Ok(())
    }
}

/// A minter seed from OS-seeded hasher state, as the CLI makes one.
fn seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::hash::RandomState::new().build_hasher().finish()
}
