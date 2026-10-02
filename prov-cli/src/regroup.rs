//! `prov gather` and `prov scatter` — attachments into one manifest, and a
//! manifest back into attachments. See `prov::Workspace::gather`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use prov::{Loss, LossKind, RegroupOptions, block_on, link};

use crate::CmdResult;
use crate::check::resolve_manifest_node;
use crate::session::{Session, resolve_target, ws_rel};

/// One line per loss, marking the ones a `--discard` would accept.
fn report(losses: &[Loss], options: &RegroupOptions) {
    for loss in losses {
        let accepted = matches!(&loss.what, LossKind::Field(key) if options.discard.contains(key));
        let line = match &loss.what {
            LossKind::Field(key) => format!(
                "{}: field `{key}`{}",
                loss.record.display(),
                if accepted {
                    " (discarded)"
                } else {
                    " — keep it, or --discard it"
                }
            ),
            LossKind::Linked { from, to } => format!(
                "{}: linked to from {} — remove the link first",
                to.display(),
                from.display()
            ),
            LossKind::Children(n) => format!(
                "{}: contains {n} document(s) — move them first",
                loss.record.display()
            ),
        };
        eprintln!("  would lose {line}");
    }
}

pub(crate) fn cmd_gather(
    targets: &[PathBuf],
    into: &Path,
    title: Option<&str>,
    discard: Vec<String>,
    dry_run: bool,
) -> CmdResult {
    let mut session = if dry_run {
        Session::open()?
    } else {
        Session::open_for_mutation()?
    };
    // A card may be named by its sidecar or by the file it describes.
    let mut cards = Vec::new();
    for target in targets {
        let rel = ws_rel(&session.ctx, target)?;
        let card = match block_on(session.ws.attachment_for(&rel))? {
            Some(sidecar) => sidecar,
            None => rel,
        };
        cards.push(card);
    }
    let dir = ws_rel(&session.ctx, into)?;
    let title = title
        .map(str::to_owned)
        .unwrap_or_else(|| link::path_to_title(&dir));
    let options = RegroupOptions { discard };

    let plan = block_on(session.ws.plan_gather(&cards, &dir))?;
    report(&plan.losses, &options);
    let blocked = !plan.blockers(&options).is_empty();
    if dry_run || blocked {
        for file in &plan.files {
            eprintln!(
                "  would move {} -> {}",
                file.from.display(),
                file.to.display()
            );
        }
        for key in &plan.carried {
            eprintln!("  would carry `{key}` onto {}", plan.node.display());
        }
        if blocked {
            return Err("nothing gathered".into());
        }
        eprintln!(
            "would gather {} file(s) into {} under {}",
            plan.files.len(),
            plan.node.display(),
            plan.parent.display()
        );
        return Ok(ExitCode::SUCCESS);
    }
    let plan = block_on(session.ws.gather(&cards, &dir, &title, &options))?;
    session.commit()?;
    eprintln!(
        "gathered {} file(s) into {} under {}",
        plan.files.len(),
        plan.root.display(),
        plan.parent.display()
    );
    println!("{}", plan.node.display());
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_scatter(
    target: &Path,
    into: Option<&str>,
    discard: Vec<String>,
    dry_run: bool,
) -> CmdResult {
    let mut session = if dry_run {
        Session::open()?
    } else {
        Session::open_for_mutation()?
    };
    let target_rel = ws_rel(&session.ctx, target)?;
    let node = resolve_manifest_node(&session.ws, &target_rel)?;
    let into = match into {
        Some(t) => Some(ws_rel(&session.ctx, &resolve_target(t)?)?),
        None => None,
    };
    let options = RegroupOptions { discard };

    let plan = block_on(session.ws.plan_scatter(&node, into.as_deref()))?;
    report(&plan.losses, &options);
    let blocked = !plan.blockers(&options).is_empty();
    if dry_run || blocked {
        for (card, _) in &plan.cards {
            eprintln!("  would write {}", card.display());
        }
        if blocked {
            return Err("nothing scattered".into());
        }
        eprintln!(
            "would scatter {} file(s) under {}",
            plan.cards.len(),
            plan.into.display()
        );
        return Ok(ExitCode::SUCCESS);
    }
    let plan = block_on(session.ws.scatter(&node, into.as_deref(), &options))?;
    session.commit()?;
    eprintln!(
        "scattered {} file(s) under {}{}",
        plan.cards.len(),
        plan.into.display(),
        if plan.keeps_node {
            " (the node is now their index)"
        } else {
            ""
        }
    );
    for (card, _) in &plan.cards {
        println!("{}", card.display());
    }
    Ok(ExitCode::SUCCESS)
}
