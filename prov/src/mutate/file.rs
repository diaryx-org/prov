//! `file` — finding, or making, the index a new record belongs under.
//!
//! A filing entry (`filing.<name>`: `under`, `field`, `nest`) describes where a
//! record goes, and [`FilingSpec::route`] cuts a record's value into the index
//! titles it names — `["2026", "2026-07"]`. Until this verb, what happened next
//! was every frontend's to write: resolve the anchor, look for each index among
//! the children of the one above, make the ones that are missing, and stamp
//! each new one so the next look finds it. Three frontends meant three answers
//! to "is this the July index", and the one the writer of the next record got
//! decided whether July grew a twin.
//!
//! [`Workspace::file`] is that walk, once. It returns the **container** and
//! leaves the record to the caller: the record's name, grammar and opening
//! fields are the caller's to choose, and a scoped field's starting value
//! depends on the container, so the record can only be composed once the
//! container is known. What prov owns is the chain above it, which lands as
//! one change set — a crash leaves no half-made month under a year that is not
//! there. The record is a second write; a crash between the two leaves an empty
//! index, which the next filing finds rather than duplicates.
//!
//! ## Which child is the index
//!
//! By the value it carries before the title it shows. A period index is the
//! child of the index above whose own value, read through the entry's field
//! chain and cut at this level, is the period: a month index titled `March
//! 2026`, `2026-03 index` or `03` carries `2026-03` whatever its author called
//! it, and a title rule that matched all three would also match things it
//! should not. A child that carries no value at all is matched by title, the
//! way route addressing matches — which is how a hand-made `2026` index, or an
//! `initial` index (whose field is a name, not a thing to write on an index),
//! is found. A child whose value names a *different* period is not the index
//! whatever it is titled.
//!
//! An index this verb makes carries both: its title is the period, and, for a
//! calendar grain, the period is written into the head of the entry's field
//! chain (`date_of_document: 2026-07`) — a reduced-precision date, which is
//! what a month *is*. It is made in a directory of its own named for the
//! period's last component (`2026/07/index.md`, beside `06/`), because it will
//! hold records.

use std::path::{Path, PathBuf};

use crate::filing::{FilingSpec, Nest};
use crate::grain::Grain;
use crate::identity::IdentityPolicy;
use crate::workspace::Workspace;
use prov_graph::content::ContentFormat;
use prov_graph::document::Document;
use prov_graph::error::{Error, Result};
use prov_graph::link;
use prov_graph::meta::{Mapping, Value};
use prov_store::fs::Storage;
use prov_store::index::IndexStore;

/// What [`Workspace::plan_file`] found: where a record with the given value
/// goes, and which indexes filing it there needs made. Nothing is written
/// until it is [applied](Workspace::apply_file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilingPlan {
    /// The entry's anchor, resolved — the root when it names none.
    pub anchor: PathBuf,
    /// The index titles walked below the anchor, coarsest first —
    /// `["2026", "2026-07"]`. Empty when the record files directly under the
    /// anchor, or under the document its value links to.
    pub route: Vec<String>,
    /// The indexes to make, parents first. Empty when every one exists.
    pub create: Vec<PlannedIndex>,
    /// The container the record belongs in once the plan is applied.
    pub parent: PathBuf,
}

/// One index a [`FilingPlan`] makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedIndex {
    /// Where it is written.
    pub path: PathBuf,
    /// The index it is made under.
    pub parent: PathBuf,
    /// Its title — the period, `2026-07`.
    pub title: String,
    /// The field it carries the period in, when it carries one — the head of
    /// the entry's field chain, for a calendar grain.
    pub field: Option<String>,
}

/// What [`Workspace::file`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filed {
    /// The container the new record belongs in — create it there.
    pub parent: PathBuf,
    /// The indexes made on the way, parents first.
    pub created: Vec<PathBuf>,
}

impl<FS: Storage, IdP, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Where a record whose metadata is `meta` files under `spec`, and what
    /// has to be made for it to — read only, so a caller can say what is about
    /// to happen ("making 2026-07") before it does.
    ///
    /// `root_doc` is the root the entry's anchor is resolved from, as a scoped
    /// field's is ([`anchor_path`](Self::anchor_path)); an entry with no
    /// `under:` files below the root. Then, by the entry's `nest:`:
    ///
    /// - **none** — directly under the anchor;
    /// - **`ref`** — under the document the record's value links to, resolved
    ///   the same way; a record that names none yet files under the anchor
    ///   until it does;
    /// - **a grain** — through one index per level of the grain's chain,
    ///   each found or planned as the module docs describe.
    ///
    /// A value the grain cannot cut all the way down files at the deepest
    /// level it reaches: `2026-08`, under a `day` entry, belongs in August and
    /// in no day of it, and a value that reaches no level — or a record with
    /// no value, an undated scan — files directly under the anchor, in the
    /// archive and in no period. [`FilingSpec::route`] declines both, because
    /// a route is the entry's *full* depth; filing a real record has to put it
    /// somewhere, and the honest somewhere is as deep as its value says.
    ///
    /// An error when the anchor or a `ref` link names nothing, when the record
    /// carries several values for the field (two homes, and the spine allows
    /// one), when two children both claim to be the same index, or when an
    /// index would have to be made where something already stands.
    pub async fn plan_file(
        &self,
        root_doc: &Path,
        spec: &FilingSpec,
        meta: &Value,
    ) -> Result<FilingPlan> {
        let root_doc = link::normalize(root_doc);
        let mut titles = None;
        let anchor = match &spec.under {
            None => root_doc.clone(),
            Some(under) => self
                .anchor_path(&root_doc, under, &mut titles)
                .await?
                .map_err(|why| {
                    Error::Structure(format!(
                        "filing `{}` files under {under}, which names nothing: {why}",
                        spec.name
                    ))
                })?,
        };
        let flat = |parent: PathBuf| FilingPlan {
            anchor: anchor.clone(),
            route: Vec::new(),
            create: Vec::new(),
            parent,
        };
        let Some(nest) = spec.nest else {
            return Ok(flat(anchor.clone()));
        };
        let values = crate::filing::chain_values(meta, &spec.field);
        let value = match values.as_slice() {
            [] => return Ok(flat(anchor.clone())),
            [one] => one.clone(),
            many => {
                return Err(Error::Structure(format!(
                    "filing `{}`: the record carries {} values for {} ({}), and a record \
                     has one place in the tree",
                    spec.name,
                    many.len(),
                    spec.field.join(" / "),
                    many.join(", ")
                )));
            }
        };
        let grain = match nest {
            Nest::Grain(grain) => grain,
            Nest::Ref => {
                let target = self
                    .anchor_path(&root_doc, &value, &mut titles)
                    .await?
                    .map_err(|why| {
                        Error::Structure(format!(
                            "filing `{}`: the record files under {value}, which names nothing: \
                             {why}",
                            spec.name
                        ))
                    })?;
                return Ok(flat(target));
            }
        };

        let route: Vec<(Grain, String)> = grain
            .chain()
            .into_iter()
            .map_while(|level| level.cut(&value).map(|period| (level, period)))
            .collect();
        let ext = anchor
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| ContentFormat::from_extension(Path::new(&format!("x.{e}"))).is_some())
            .unwrap_or("md")
            .to_string();
        let head = spec
            .field
            .first()
            .filter(|key| !key.contains("[]"))
            .cloned();

        let mut parent = anchor.clone();
        let mut create: Vec<PlannedIndex> = Vec::new();
        for (level, period) in &route {
            if create.is_empty()
                && let Some(found) = self.period_index(&parent, spec, *level, period).await?
            {
                parent = found;
                continue;
            }
            if create.is_empty() {
                self.assert_combined(&parent).await?;
            }
            let path = period_path(&parent, *level, period, &ext);
            self.assert_vacant(&path, period, crate::route::Layout::Nested)
                .await?;
            create.push(PlannedIndex {
                path: path.clone(),
                parent: parent.clone(),
                title: period.clone(),
                field: head.clone().filter(|_| is_calendar(*level)),
            });
            parent = path;
        }
        Ok(FilingPlan {
            anchor,
            route: route.into_iter().map(|(_, period)| period).collect(),
            create,
            parent,
        })
    }

    /// The child of `parent` that is the index for `period` at `level`: the
    /// one whose own value, read through the entry's chain, cuts to it — or,
    /// among the children that carry no value, the one titled it.
    async fn period_index(
        &self,
        parent: &Path,
        spec: &FilingSpec,
        level: Grain,
        period: &str,
    ) -> Result<Option<PathBuf>> {
        let mut by_value: Vec<(PathBuf, Document)> = Vec::new();
        let mut by_title: Vec<PathBuf> = Vec::new();
        for (path, child) in self.spanning_children(parent).await? {
            let values = crate::filing::chain_values(&child.meta, &spec.field);
            match values.as_slice() {
                [] => {
                    if child
                        .meta
                        .get("title")
                        .and_then(prov_graph::title::title_text)
                        .as_deref()
                        == Some(period)
                    {
                        by_title.push(path);
                    }
                }
                [one] if level.cut(one).as_deref() == Some(period) => by_value.push((path, child)),
                _ => {}
            }
        }
        // Several children can cut to one period — a month index, and an
        // entry misfiled beside it that is dated in that month. The one that
        // says the period exactly is the index; failing that, the one that
        // holds children.
        if by_value.len() > 1 {
            let exact: Vec<usize> = (0..by_value.len())
                .filter(|&i| {
                    crate::filing::chain_values(&by_value[i].1.meta, &spec.field)
                        .first()
                        .is_some_and(|v| v == period)
                })
                .collect();
            let containers: Vec<usize> = (0..by_value.len())
                .filter(|&i| {
                    self.relations()
                        .spanning_relation()
                        .is_some_and(|s| by_value[i].1.meta.get(s).is_some())
                })
                .collect();
            for pick in [exact, containers] {
                if let [one] = pick.as_slice() {
                    return Ok(Some(by_value.swap_remove(*one).0));
                }
            }
            return Err(Error::Structure(format!(
                "{} has {} children that are the index for {period} ({}); filing cannot say \
                 which",
                parent.display(),
                by_value.len(),
                by_value
                    .iter()
                    .map(|(p, _)| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if let Some((path, _)) = by_value.pop() {
            return Ok(Some(path));
        }
        match by_title.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(one.clone())),
            many => Err(Error::Structure(format!(
                "{} has {} children titled {period:?} ({}); filing cannot say which",
                parent.display(),
                many.len(),
                many.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }
}

impl<FS: Storage, IdP: IdentityPolicy, Ix: IndexStore> Workspace<FS, IdP, Ix> {
    /// Make the indexes `plan` names, as one change set, and return where the
    /// record goes. Each is made as `create` makes a document — linked both
    /// ways, in its parent's grammar, registered when the identity policy says
    /// so — titled with its period, carrying the period in its field when the
    /// plan says it does, and opening with `opening` besides (a creation
    /// stamp, say; prov reads no clock). A plan with nothing to make writes
    /// nothing.
    pub async fn apply_file(&mut self, plan: &FilingPlan, opening: &Mapping) -> Result<Filed> {
        if plan.create.is_empty() {
            return Ok(Filed {
                parent: plan.parent.clone(),
                created: Vec::new(),
            });
        }
        let mut cs = self.change();
        let mut created = Vec::new();
        for index in &plan.create {
            let mut fields = opening.clone();
            if let Some(field) = &index.field {
                fields.shift_remove(field.as_str());
                prov_graph::meta::insert_path(
                    &mut fields,
                    field,
                    Value::String(index.title.clone()),
                );
            }
            let made = self
                .stage_create(
                    &mut cs,
                    &index.path,
                    &index.parent,
                    Some(&index.title),
                    &fields,
                )
                .await?;
            created.push(made.node);
        }
        self.commit(cs).await?;
        Ok(Filed {
            parent: plan.parent.clone(),
            created,
        })
    }

    /// Find or make the container a record whose metadata is `meta` belongs
    /// in under `spec`: [`plan_file`](Self::plan_file), then
    /// [`apply_file`](Self::apply_file). See the module docs for why the
    /// record itself is left to the caller.
    pub async fn file(
        &mut self,
        root_doc: &Path,
        spec: &FilingSpec,
        meta: &Value,
        opening: &Mapping,
    ) -> Result<Filed> {
        let plan = self.plan_file(root_doc, spec, meta).await?;
        self.apply_file(&plan, opening).await
    }

    /// [`file`](Self::file) by the name of an entry the workspace declares.
    /// An error when it declares none of that name.
    pub async fn file_by(
        &mut self,
        root_doc: &Path,
        filing: &str,
        meta: &Value,
        opening: &Mapping,
    ) -> Result<Filed> {
        let config = self.effective_config(root_doc).await?;
        let Some(spec) = config.filing.iter().find(|f| f.name == filing) else {
            return Err(Error::Structure(format!(
                "this workspace declares no filing entry named `{filing}`"
            )));
        };
        self.file(root_doc, spec, meta, opening).await
    }
}

/// Whether a grain's periods are dates — and so something an index can carry
/// in a date field. An initial is a cut of a name, and writing `A` into an
/// index's `surname` would say something false about it.
fn is_calendar(grain: Grain) -> bool {
    matches!(grain, Grain::Year | Grain::Month | Grain::Day)
}

/// Where an index for `period` is made below `parent`: a directory of its
/// own beside `parent`'s file, named for the period's last component — `07`
/// for `2026-07`, so a year's months sit side by side as `06/`, `07/` — with
/// the index inside it. An initial is slugged whole.
fn period_path(parent: &Path, grain: Grain, period: &str, ext: &str) -> PathBuf {
    let dir = parent.parent().unwrap_or(Path::new(""));
    let name = if is_calendar(grain) {
        period.rsplit('-').next().unwrap_or(period).to_string()
    } else {
        link::slug(period)
    };
    link::normalize(dir.join(name).join(format!("index.{ext}")))
}

#[cfg(all(test, feature = "yaml"))]
mod tests {
    use super::*;
    use prov_graph::exec::block_on;
    use prov_graph::fs::StdFs;
    use prov_testkit::{read, write};

    fn tempdir(tag: &str) -> PathBuf {
        prov_testkit::scratch("file", tag)
    }

    /// A workspace with a `Daily` index and a `daily` entry nesting under it
    /// at `nest` by `[date_of_document, created]`.
    fn journal(tag: &str, nest: &str) -> (PathBuf, Workspace<StdFs>) {
        let dir = tempdir(tag);
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\nconfig: prov.yaml\ncontents:\n- daily/index.md\n---\n",
        );
        write(
            &dir,
            "prov.yaml",
            format!(
                "filing:\n  daily:\n    under: '[[Daily]]'\n    field: [date_of_document, created]\n    nest: {nest}\n"
            ),
        );
        write(
            &dir,
            "daily/index.md",
            "---\ntitle: Daily\npart_of: ../index.md\n---\n",
        );
        let ws = Workspace::builder(StdFs).root(&dir).build();
        (dir, ws)
    }

    fn meta(yaml: &str) -> Value {
        prov_graph::meta::parse_value(yaml, prov_graph::Format::Yaml).unwrap()
    }

    fn spec(ws: &Workspace<StdFs>) -> FilingSpec {
        let config = block_on(ws.effective_config(Path::new("index.md"))).unwrap();
        config.filing[0].clone()
    }

    #[test]
    fn a_dated_record_files_through_its_year_and_month() {
        let (dir, mut ws) = journal("month", "month");
        let spec = spec(&ws);
        let plan =
            block_on(ws.plan_file(Path::new("index.md"), &spec, &meta("created: 2026-07-24\n")))
                .unwrap();
        assert_eq!(plan.anchor, Path::new("daily/index.md"));
        assert_eq!(plan.route, ["2026", "2026-07"]);
        assert_eq!(plan.create.len(), 2);

        let filed = block_on(ws.apply_file(&plan, &Mapping::new())).unwrap();
        assert_eq!(filed.parent, Path::new("daily/2026/07/index.md"));
        assert_eq!(
            filed.created,
            [
                PathBuf::from("daily/2026/index.md"),
                PathBuf::from("daily/2026/07/index.md")
            ]
        );
        let month = read(&dir, "daily/2026/07/index.md");
        assert!(
            month.contains("title: 2026-07") && month.contains("date_of_document: 2026-07"),
            "{month}"
        );
        assert!(read(&dir, "daily/index.md").contains("2026/index.md"));
        assert!(
            block_on(ws.check("index.md")).unwrap().is_empty(),
            "{:#?}",
            block_on(ws.check("index.md")).unwrap()
        );

        // The next record in the same month finds both and makes nothing.
        let again = block_on(ws.file(
            Path::new("index.md"),
            &spec,
            &meta("date_of_document: 2026-07-01\n"),
            &Mapping::new(),
        ))
        .unwrap();
        assert_eq!(again.parent, filed.parent);
        assert!(again.created.is_empty());
    }

    /// Found by the value it carries, not by what it is called: a month index
    /// retitled by its author is still July.
    #[test]
    fn an_index_is_found_by_its_value_before_its_title() {
        let (dir, mut ws) = journal("by-value", "month");
        let spec = spec(&ws);
        let first = block_on(ws.file(
            Path::new("index.md"),
            &spec,
            &meta("created: 2026-07-24\n"),
            &Mapping::new(),
        ))
        .unwrap();
        let month = read(&dir, "daily/2026/07/index.md").replace("title: 2026-07", "title: July");
        write(&dir, "daily/2026/07/index.md", &month);
        let again = block_on(ws.file(
            Path::new("index.md"),
            &spec,
            &meta("created: 2026-07-30\n"),
            &Mapping::new(),
        ))
        .unwrap();
        assert_eq!(again.parent, first.parent);

        // A hand-made index carrying no date is found by its title — `title:
        // 2026` is an int to YAML, and still the title "2026".
        write(
            &dir,
            "daily/index.md",
            "---\ntitle: Daily\npart_of: ../index.md\ncontents:\n- 2026/index.md\n- y2025.md\n---\n",
        );
        write(
            &dir,
            "daily/y2025.md",
            "---\ntitle: 2025\npart_of: index.md\n---\n",
        );
        let plan =
            block_on(ws.plan_file(Path::new("index.md"), &spec, &meta("created: 2025-03-02\n")))
                .unwrap();
        assert_eq!(plan.create.len(), 1, "{plan:#?}");
        assert_eq!(plan.create[0].parent, Path::new("daily/y2025.md"));
    }

    /// A value the grain reaches only partway files as deep as it reaches;
    /// no value files under the anchor; two values are refused.
    #[test]
    fn a_record_files_as_deep_as_its_value_says() {
        let (_, ws) = journal("partial", "day");
        let spec = spec(&ws);
        let plan = |yaml: &str| block_on(ws.plan_file(Path::new("index.md"), &spec, &meta(yaml)));
        assert_eq!(
            plan("created: 2026-08\n").unwrap().route,
            ["2026", "2026-08"]
        );
        let undated = plan("title: A scan\n").unwrap();
        assert_eq!(undated.parent, Path::new("daily/index.md"));
        assert!(undated.create.is_empty() && undated.route.is_empty());
        assert!(plan("created: [2026-08-01, 2026-08-02]\n").is_err());
    }

    /// `nest: ref` files under the document the record names, and an entry
    /// with no `nest:` directly under its anchor.
    #[test]
    fn a_reference_files_under_what_it_names() {
        let dir = tempdir("ref");
        write(
            &dir,
            "index.md",
            "---\ntitle: Home\nconfig: prov.yaml\ncontents:\n- shelf.md\n---\n",
        );
        write(
            &dir,
            "prov.yaml",
            "fields:\n  shelf:\n    type: ref\nfiling:\n  books:\n    field: shelf\n    nest: ref\n  loose:\n    under: shelf.md\n",
        );
        write(
            &dir,
            "shelf.md",
            "---\ntitle: Shelf\npart_of: index.md\n---\n",
        );
        let mut ws = Workspace::builder(StdFs).root(&dir).build();
        let filed = block_on(ws.file_by(
            Path::new("index.md"),
            "books",
            &meta("shelf: '[[Shelf]]'\n"),
            &Mapping::new(),
        ))
        .unwrap();
        assert_eq!(filed.parent, Path::new("shelf.md"));
        let loose = block_on(ws.file_by(
            Path::new("index.md"),
            "loose",
            &meta("{}\n"),
            &Mapping::new(),
        ))
        .unwrap();
        assert_eq!(loose.parent, Path::new("shelf.md"));
        assert!(
            block_on(ws.file_by(
                Path::new("index.md"),
                "nope",
                &meta("{}\n"),
                &Mapping::new()
            ))
            .is_err()
        );
    }

    /// The chain lands whole or not at all: a write failing anywhere in it
    /// leaves the workspace exactly as it was, with no year made and its
    /// month not.
    #[test]
    fn a_failed_filing_leaves_nothing_behind() {
        for fail_at in 1..=3 {
            let (dir, _) = journal(&format!("torn-{fail_at}"), "month");
            let before = super::super::support::snapshot(&dir);
            let mut ws = super::super::support::failing_ws(&dir, fail_at);
            let config = block_on(ws.effective_config(Path::new("index.md"))).unwrap();
            let err = block_on(ws.file(
                Path::new("index.md"),
                &config.filing[0],
                &meta("created: 2026-07-24\n"),
                &Mapping::new(),
            ))
            .unwrap_err();
            assert!(err.to_string().contains("disk full"), "{err}");
            assert_eq!(
                super::super::support::snapshot(&dir),
                before,
                "write {fail_at}"
            );
        }
    }
}
