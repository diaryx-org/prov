//! `prov-fhir` — import a FHIR export into a medical prov workspace.
//!
//! ```text
//! prov-fhir patients <SOURCE>
//! prov-fhir import <SOURCE> --into <ROOT> [--patient <ID>]
//! prov-fhir import <SOURCE> --each <DIR>
//! ```
//!
//! `SOURCE` is a Bulk Data export directory (`*.ndjson`), one ndjson file, or a
//! JSON Bundle. A workspace keeps one patient's record, so `--into` takes one
//! patient (`--patient`, which may be a unique prefix of the id, and may be
//! left out when the export holds only one), and `--each` makes a workspace
//! per patient under `DIR`. A `--into` root that does not exist yet, or is
//! empty, is made the way `--each` makes one: `prov init` with the medical
//! preset.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use prov::block_on;
use prov::preset::Preset;

use prov_fhir::Error;
use prov_fhir::fhir::{Export, patient_name};
use prov_fhir::import::{self, Options, Report};

#[derive(Parser)]
#[command(
    name = "prov-fhir",
    version,
    about = "Import a FHIR export into a medical prov workspace"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the patients an export holds, with how much each one's share is.
    Patients {
        /// A Bulk Data export directory, an ndjson file, or a Bundle.
        source: PathBuf,
    },
    /// Import one patient into a workspace, or every patient into one each.
    Import {
        /// A Bulk Data export directory, an ndjson file, or a Bundle.
        source: PathBuf,
        /// The workspace to import into: its root directory. Made, with the
        /// medical preset, when it does not exist or is empty.
        #[arg(long, conflicts_with = "each", required_unless_present = "each")]
        into: Option<PathBuf>,
        /// The patient to import (a FHIR id, or a unique prefix of one).
        /// Needed with `--into` when the export holds more than one.
        #[arg(long, conflicts_with = "each")]
        patient: Option<String>,
        /// Make one workspace per patient, each in a directory of its own
        /// under this one, named for the patient.
        #[arg(long)]
        each: Option<PathBuf>,
        /// The preset a new workspace is made with (default: the medical
        /// preset built into this binary).
        #[arg(long)]
        preset: Option<PathBuf>,
        /// Leave every document's `audience` at `self`, instead of putting
        /// active disorders, current medications and active allergies in
        /// `emergency`.
        #[arg(long)]
        no_emergency: bool,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("prov-fhir: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, Error> {
    match cli.command {
        Command::Patients { source } => {
            let export = Export::read(&source)?;
            for patient in export.patients() {
                let record = export
                    .record(patient.id())?
                    .expect("a listed patient has a record");
                println!(
                    "{}\t{}\t{}\t{} resources",
                    patient.id(),
                    patient_name(patient),
                    patient.str(&["birthDate"]).unwrap_or("-"),
                    record.resources.len()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Import {
            source,
            into,
            patient,
            each,
            preset,
            no_emergency,
        } => {
            let started = std::time::Instant::now();
            let export = Export::read(&source)?;
            eprintln!(
                "read {} in {:.1}s",
                source.display(),
                started.elapsed().as_secs_f64()
            );
            let opts = Options {
                emergency: !no_emergency,
                now: prov_fhir::now_rfc3339(),
            };
            let preset = match &preset {
                Some(dir) => Preset::load(dir)?,
                None => prov_fhir::preset::medical()?,
            };
            if let Some(out) = each {
                let ids: Vec<String> = export.patients().map(|p| p.id().to_string()).collect();
                for id in ids {
                    let record = export.record(&id)?.expect("listed");
                    let name = patient_name(&record.patient);
                    let dir = out.join(format!(
                        "{}-{}",
                        prov::link::slug(&name),
                        &id[..8.min(id.len())]
                    ));
                    let made = make_if_empty(&dir, &name, &preset)?;
                    let report = block_on(import::import(&dir, &record, &opts))?;
                    summarize(&dir, &name, made, &report);
                }
                return Ok(ExitCode::SUCCESS);
            }
            let into = into.expect("clap requires --into without --each");
            let ids: Vec<&str> = export.patients().map(|p| p.id()).collect();
            let id = match (&patient, ids.as_slice()) {
                (None, [one]) => one.to_string(),
                (None, []) => return Err(format!("{}: no Patient in it", source.display()).into()),
                (None, _) => {
                    return Err(format!(
                        "{} holds {} patients and a workspace keeps one patient's record: \
                         name one with --patient (`prov-fhir patients {}` lists them), \
                         or use --each DIR",
                        source.display(),
                        ids.len(),
                        source.display()
                    )
                    .into());
                }
                (Some(want), _) => {
                    let matches: Vec<&&str> = ids
                        .iter()
                        .filter(|i| i.starts_with(want.as_str()))
                        .collect();
                    match matches.as_slice() {
                        [one] => one.to_string(),
                        [] => {
                            return Err(format!("no patient {want} in {}", source.display()).into());
                        }
                        _ => {
                            return Err(format!(
                                "{want} names {} patients; give more of the id",
                                matches.len()
                            )
                            .into());
                        }
                    }
                }
            };
            let record = export.record(&id)?.expect("matched");
            let name = patient_name(&record.patient);
            let made = make_if_empty(&into, &name, &preset)?;
            let report = block_on(import::import(&into, &record, &opts))?;
            summarize(&into, &name, made, &report);
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Make a workspace at `dir` when nothing is there yet. Whether it did.
fn make_if_empty(dir: &Path, name: &str, preset: &Preset) -> Result<bool, Error> {
    let empty = !dir.exists() || std::fs::read_dir(dir)?.next().is_none();
    if empty {
        block_on(prov_fhir::preset::init(dir, name, preset))?;
    }
    Ok(empty)
}

fn summarize(dir: &Path, name: &str, made: bool, report: &Report) {
    println!("{} — {name}", dir.display());
    if made {
        println!("  made the workspace (medical preset)");
    }
    if !report.indexes.is_empty() {
        println!("  made indexes: {}", report.indexes.join(", "));
    }
    println!(
        "  import: {}{}",
        report.import.display(),
        if report.import_reused {
            " (already imported; reused)"
        } else {
            ""
        }
    );
    let line = |label: &str, map: &std::collections::BTreeMap<prov_fhir::map::Section, usize>| {
        if map.is_empty() {
            return;
        }
        let parts: Vec<String> = map
            .iter()
            .map(|(s, n)| format!("{} {n}", s.title()))
            .collect();
        println!("  {label} {}: {}", Report::total(map), parts.join(", "));
    };
    line("created", &report.created);
    line("updated", &report.updated);
    line("unchanged", &report.unchanged);
    if report.summarized > 0 {
        println!(
            "  {} observations summarized into encounter tables",
            report.summarized
        );
    }
    if !report.kept.is_empty() {
        let parts: Vec<String> = report
            .kept
            .iter()
            .map(|(t, n)| format!("{t} {n}"))
            .collect();
        println!("  kept in the import only: {}", parts.join(", "));
    }
    for (why, n) in &report.skipped {
        println!("  not imported: {n} × {why}");
    }
    for (system, n) in &report.unknown_systems {
        println!("  {n} coding(s) from {system}: kept in other_codes, not in codes");
    }
    println!("  {:.1}s", report.seconds);
}
