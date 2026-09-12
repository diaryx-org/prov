//! `views`, `exports`, `presets` — the declared shapes of a workspace, listed
//! and run.
//!
//! Each of these is a thing the workspace *says about itself* in its config:
//! a view is a selection and grouping, an export is a view behind a gate, a
//! preset is a bundle of config the workspace may adopt. The no-argument form
//! of each lists what is declared, because the first question about any of
//! them is whether the workspace agrees it exists — and a block that went
//! unread over a misspelled key is invisible everywhere else by design.

use std::path::Path;
use std::process::ExitCode;

use prov::{Format, Value, block_on, meta};

use crate::CmdResult;
use crate::about::refresh_about;
use crate::json;
use crate::session::{Session, find_root, workspace};

/// `prov views [NAME]` — list the declared views, or execute one.
///
/// Listing is deliberately the no-argument form. A view is a thing a workspace
/// *says about itself*, and the first question about one is whether the
/// workspace agrees it exists — which is also the fastest way to find out that
/// a `views:` block went unread because a key was misspelled (the stderr
/// warning `find_root` already prints covers the why).
///
/// `--json` is the same two answers for a program rather than a person, and is
/// what lets a consumer replace a per-file metadata loop with one view: every
/// row carries the document's whole metadata block, so the result is the
/// selection *and* what the caller went to the files for.
pub(crate) fn cmd_views(name: Option<&str>, as_json: bool) -> CmdResult {
    let ctx = find_root()?;
    let views = &ctx.config.views;
    let Some(name) = name else {
        if as_json {
            // Including the empty case, which is `[]` — "declares no views" is
            // narration for a person, and an empty array says it already.
            print!(
                "{}",
                json::J::Arr(views.iter().map(json::view).collect()).render()
            );
            return Ok(ExitCode::SUCCESS);
        }
        if views.is_empty() {
            println!("this workspace declares no views");
            return Ok(ExitCode::SUCCESS);
        }
        for view in views {
            let scope = match &view.under {
                Some(under) => format!(" under {under}"),
                None => " (whole workspace)".to_string(),
            };
            let by = match view.group.by {
                Some(grain) => format!(" by {}", grain.display()),
                None => String::new(),
            };
            // The condition is flagged, not rendered: a nested `where:` does
            // not fit a listing line, and what a reader needs from a list is
            // that this view does not show everything it reaches.
            let filtered = if view.filter.is_some() {
                " [filtered]"
            } else {
                ""
            };
            // Shown because `nest` is the half that *writes*: which lens files
            // a new record, and how deep, is worth seeing without opening the
            // config.
            let nest = match view.nest {
                Some(grain) => format!(", files by {}", grain.display()),
                None => String::new(),
            };
            println!(
                "{}  {} — group: {}{by}{scope}{filtered}{nest}",
                view.name,
                view.display_label(),
                view.group.keys.join(" → "),
            );
        }
        return Ok(ExitCode::SUCCESS);
    };

    let Some(view) = views.iter().find(|v| v.name == name) else {
        let declared: Vec<&str> = views.iter().map(|v| v.name.as_str()).collect();
        return Err(format!(
            "no view named `{name}` — this workspace declares {}",
            if declared.is_empty() {
                "none".to_string()
            } else {
                declared.join(", ")
            }
        )
        .into());
    };

    let ws = workspace(&ctx)?;
    let selection = block_on(ws.select_view(&ctx.root_doc, view))?;
    let rows = prov::views::group(&selection, &view.group);
    if as_json {
        print!("{}", json::view_result(&selection, &rows).render());
        return Ok(ExitCode::SUCCESS);
    }
    for group in &rows.groups {
        println!("{} ({})", group.key, group.rows.len());
        for row in &group.rows {
            print_view_row(row);
        }
    }
    // Named rather than silently omitted: a view whose entries have all stopped
    // grouping looks exactly like an empty archive, and the difference is the
    // whole diagnosis.
    if !rows.ungrouped.is_empty() {
        println!("(ungrouped) ({})", rows.ungrouped.len());
        for row in &rows.ungrouped {
            print_view_row(row);
        }
    }
    // The document count, not the row count: a document under two of a
    // multi-valued field's groups is one document in two places, and a total
    // that counted it twice would claim the view covers more than the
    // workspace holds.
    match selection.len() {
        0 => println!("no documents in scope"),
        n => println!("\n{n} document(s), {} row(s)", rows.placements()),
    }
    Ok(ExitCode::SUCCESS)
}

/// One row of a view: `  path — title`.
fn print_view_row(row: &prov::views::Row) {
    match row.title() {
        Some(title) => println!("  {} — {title}", row.path.display()),
        None => println!("  {}", row.path.display()),
    }
}

/// `prov exports [NAME]` — list the declared exports, or preview one's plan.
///
/// Listing is the no-argument form for the same reason `views` lists: the
/// first question about an export is whether the workspace agrees it exists,
/// and an `exports:` entry that went unread (misspelled gate, stray key) is
/// invisible everywhere else *by design* — parse dropping it is the
/// fail-closed direction, and this listing plus the config lint are where the
/// silence is broken.
///
/// A preview moves nothing. It prints all three sides of the boundary — what
/// leaves, what the gate held back, what the view scoped out — because the
/// question a preview answers is "why isn't this file in the export?", and
/// the answer differs by which side the file is on.
pub(crate) fn cmd_exports(name: Option<&str>) -> CmdResult {
    let ctx = find_root()?;
    let exports = &ctx.config.exports;
    let Some(name) = name else {
        if exports.is_empty() {
            println!("this workspace declares no exports");
            return Ok(ExitCode::SUCCESS);
        }
        for export in exports {
            let hold = match &export.hold {
                Some(field) => format!(", holding {field}: true"),
                None => String::new(),
            };
            let view = match &export.view {
                Some(view) => format!(", arranged by {view}"),
                None => String::new(),
            };
            println!(
                "{}  {} — gate: {}: {}{hold}{view}",
                export.name,
                export.display_label(),
                export.gate.field,
                export.gate.value,
            );
        }
        return Ok(ExitCode::SUCCESS);
    };

    let Some(export) = exports.iter().find(|e| e.name == name) else {
        let declared: Vec<&str> = exports.iter().map(|e| e.name.as_str()).collect();
        return Err(format!(
            "no export named `{name}` — this workspace declares {}",
            if declared.is_empty() {
                "none".to_string()
            } else {
                declared.join(", ")
            }
        )
        .into());
    };

    let ws = workspace(&ctx)?;
    let plan = block_on(prov::exports::plan(
        ws.graph(),
        export,
        &ctx.config.views,
        &ctx.root_doc,
    ))?;

    for doc in &plan.entries {
        match &doc.title {
            Some(title) => println!("  {} — {title}", doc.path.display()),
            None => println!("  {}", doc.path.display()),
        }
    }
    // The export's pending set: admitted, and waiting on the document's own
    // word. Listed in full, with titles, because these are the documents the
    // author is closest to finishing and the ones a preview is most often
    // asked about.
    if !plan.held.is_empty() {
        println!(
            "(admitted by the gate, held back by `{}: true`) ({})",
            export.hold.as_deref().unwrap_or_default(),
            plan.held.len()
        );
        for doc in &plan.held {
            match &doc.title {
                Some(title) => println!("  {} — {title}", doc.path.display()),
                None => println!("  {}", doc.path.display()),
            }
        }
    }
    // Named because it is the difference between the export and its gate:
    // "I tagged it and it isn't in the export" is unexplainable from the
    // file alone, and this list is the explanation.
    if !plan.outside_view.is_empty() {
        println!(
            "(admitted by the gate, outside the view) ({})",
            plan.outside_view.len()
        );
        for path in &plan.outside_view {
            println!("  {}", path.display());
        }
    }
    println!(
        "\n{} document(s) leave, {} held back by the gate{}{}",
        plan.entries.len(),
        plan.withheld.len(),
        match plan.held.len() {
            0 => String::new(),
            n => format!(", {n} held by the document"),
        },
        match plan.outside_view.len() {
            0 => String::new(),
            n => format!(", {n} outside the view"),
        }
    );
    Ok(ExitCode::SUCCESS)
}

/// `prov presets [DIR] [--write]` — what a preset would write here, and, with
/// `--write`, writing it.
///
/// The plan is printed either way, one line per entry and store, so that the
/// question "what would this do to my config?" is answered before anything
/// moves — the same shape `exports <name>` gives an export. A collision is a
/// refusal with the plan still printed, because the plan is the diagnosis.
pub(crate) fn cmd_presets(dir: Option<&Path>, write: bool) -> CmdResult {
    let preset = match dir {
        Some(dir) => prov::preset::Preset::load(dir)?,
        None => prov::preset::Preset::builtin(),
    };
    let mut session = Session::open()?;
    let plan = block_on(session.ws.plan_preset(&session.ctx.root_doc, &preset))?;
    print_plan(&plan);

    if !plan.is_clean() {
        eprintln!(
            "{} collision(s): the workspace already declares these differently, and \
             prov cannot choose — nothing written",
            plan.collisions().count()
        );
        return Ok(ExitCode::FAILURE);
    }
    if !plan.writes_anything() {
        eprintln!("nothing to write — the workspace already carries this preset");
        return Ok(ExitCode::SUCCESS);
    }
    if !write {
        eprintln!("nothing written; pass --write to apply");
        return Ok(ExitCode::SUCCESS);
    }
    block_on(session.ws.apply_preset(&session.ctx.root_doc, &preset))?;
    let entries = plan
        .steps
        .iter()
        .filter(|s| matches!(s, prov::preset::Step::Set { .. }))
        .count();
    let files = plan
        .steps
        .iter()
        .filter(|s| matches!(s, prov::preset::Step::Write { .. }))
        .count();
    eprintln!(
        "wrote {entries} config entr{} into {} and {files} file(s)",
        if entries == 1 { "y" } else { "ies" },
        plan.surface.display()
    );
    // The page is a function of the config, and the config just grew.
    refresh_about(&session.ctx.root_dir)?;
    Ok(ExitCode::SUCCESS)
}

/// One line per step: `+` for what would be written, `=` for what is already
/// so, `!` for a collision. Config entries first, under the document they go
/// into; then the stores.
fn print_plan(plan: &prov::preset::Plan) {
    use prov::preset::Step;
    let config: Vec<&Step> = plan
        .steps
        .iter()
        .filter(|s| {
            matches!(
                s,
                Step::Set { .. } | Step::Same { .. } | Step::Differs { .. }
            )
        })
        .collect();
    if !config.is_empty() {
        println!(
            "{}{}",
            plan.surface.display(),
            if plan.in_root_block {
                " (the `prov:` block)"
            } else {
                ""
            }
        );
        for step in config {
            match step {
                Step::Set { key, value } => println!("  + {key}{}", scalar_suffix(value)),
                Step::Same { key } => println!("  = {key}  (already so)"),
                Step::Differs { key, .. } => println!("  ! {key}  (declared differently)"),
                _ => {}
            }
        }
    }
    for step in &plan.steps {
        match step {
            Step::Write { path } => println!("+ {}", path.display()),
            Step::Present { path } => println!("= {}  (already there)", path.display()),
            Step::Occupied { path } => println!("! {}  (exists, differs)", path.display()),
            _ => {}
        }
    }
}

/// ` = value` for a scalar, nothing for a block — a `fields.status` entry is
/// named, not printed, since its shape is in the preset for anyone to read.
fn scalar_suffix(value: &Value) -> String {
    match value {
        Value::Mapping(_) | Value::Sequence(_) => String::new(),
        Value::String(s) => format!(" = {s}"),
        other => format!(
            " = {}",
            meta::serialize_value(other, Format::Yaml)
                .unwrap_or_default()
                .trim_end()
        ),
    }
}
