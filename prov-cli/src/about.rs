//! `about` — the generated page describing the workspace, and keeping it
//! current.
//!
//! The page is a function of the config and of the root's pointer targets,
//! and of nothing else. [`refresh_about`] is the one trigger: every command
//! that writes config or bootstraps a machinery file calls it afterwards, and
//! it re-discovers the workspace so it describes the *new* policy. It is
//! best-effort by design — DESIGN §5's "what can be rebuilt need not be
//! transactional" — so a stale page costs a `check` finding, never a failed
//! config write.

use std::path::Path;
use std::process::ExitCode;

use prov::{StdFs, Workspace, block_on};

use crate::session::{Ctx, find_root, find_root_quiet_at, workspace};
use crate::{AnyError, CmdResult};

/// Build the [`AboutContext`] for this workspace — the root's name and its
/// resolved pointer targets, which is everything the generator needs that is not
/// already in the config.
pub(crate) fn about_context(ctx: &Ctx) -> Result<prov::AboutContext, AnyError> {
    let probe: Workspace<StdFs> = Workspace::builder(StdFs)
        .root(&ctx.root_dir)
        .relations(ctx.config.relation_set())
        .build();
    Ok(prov::AboutContext {
        root_doc: ctx.root_doc.clone(),
        config_doc: block_on(probe.config_path(&ctx.root_doc))?,
        registry_doc: block_on(probe.registry_path(&ctx.root_doc))?,
        deletions_doc: block_on(probe.deletions_path(&ctx.root_doc))?,
        history_doc: block_on(probe.history_path(&ctx.root_doc))?,
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Regenerate `about.md`, or inspect what would be generated.
///
/// Not gated on the `about` axis: `--print` and `--check` are read-only, and a
/// bare `prov about` in a workspace with `about: off` is a clear enough request
/// to be worth honoring — but it says so, because the page it just wrote will
/// not be maintained.
pub(crate) fn cmd_about(check: bool, print: bool) -> CmdResult {
    let ctx = find_root()?;
    let ws = workspace(&ctx)?;
    let about_ctx = about_context(&ctx)?;

    if print {
        print!(
            "{}",
            prov::about::generate(&ctx.config, ws.relations(), &about_ctx)?
        );
        return Ok(ExitCode::SUCCESS);
    }

    if check {
        let Some(diff) = block_on(ws.about_diff(&ctx.root_doc, &ctx.config, &about_ctx))? else {
            eprintln!("{} is current", diff_path_display(&ctx, None));
            return Ok(ExitCode::SUCCESS);
        };
        match &diff.actual {
            None => eprintln!("{}: missing", diff.path.display()),
            Some(_) => eprintln!("{}: stale", diff.path.display()),
        }
        eprintln!("regenerate it with `prov about`");
        return Ok(ExitCode::FAILURE);
    }

    if !prov::about::enabled(&ctx.config) {
        eprintln!(
            "note: `about` is off for this workspace, so nothing will keep this \
             page current — turn it on with `prov config about structure`"
        );
    }
    let path = block_on(ws.write_about(&ctx.root_doc, &ctx.config, &about_ctx))?;
    eprintln!("wrote {}", path.display());
    println!("{}", path.display());
    Ok(ExitCode::SUCCESS)
}

/// The page's path for a message, when there may be no diff to name it.
fn diff_path_display(ctx: &Ctx, path: Option<&Path>) -> String {
    match path {
        Some(path) => path.display().to_string(),
        None => prov::about::default_about_name(ctx.config.content_format),
    }
}

/// Bring `about.md` back in line after a config write — the one trigger that
/// matters, because the page is a function of configuration and of nothing else.
///
/// Re-discovers the workspace rather than reusing the caller's [`Ctx`]: the
/// config has just changed on disk, and the page must describe the *new* policy.
///
/// Deliberately best-effort. DESIGN §5's rule applies directly — "what can be
/// rebuilt need not be transactional" — so a failure here costs a `check`
/// finding and an easy `prov about`, never a failed config write. A config
/// change that succeeded must not be reported as failed because a derived file
/// could not be refreshed.
pub(crate) fn refresh_about(root_dir: &Path) -> Result<(), AnyError> {
    let ctx = find_root_quiet_at(root_dir)?;
    let ws = workspace(&ctx)?;
    if prov::about::enabled(&ctx.config) {
        let about_ctx = about_context(&ctx)?;
        // Write only when the page would actually change. Most config writes
        // move an axis the page does not mention, and a derived file that is
        // rewritten with identical bytes is a sync transport's problem for no
        // reader's benefit.
        match block_on(ws.about_diff(&ctx.root_doc, &ctx.config, &about_ctx)) {
            Ok(None) => return Ok(()),
            Ok(Some(_)) => {}
            Err(e) => {
                eprintln!("prov: could not check about.md ({e}); run `prov about`");
                return Ok(());
            }
        }
        match block_on(ws.write_about(&ctx.root_doc, &ctx.config, &about_ctx)) {
            Ok(path) => eprintln!("regenerated {}", path.display()),
            Err(e) => eprintln!("prov: could not regenerate about.md ({e}); run `prov about`"),
        }
        return Ok(());
    }
    // `structure` → `off`: the page and its pointer go. Safe to delete outright
    // and deliberately *not* routed to the recycle bin — the page is derived, so
    // there is nothing to recover that regenerating would not reproduce.
    match block_on(ws.remove_about(&ctx.root_doc)) {
        Ok(Some(path)) => eprintln!("removed {} (about is off)", path.display()),
        Ok(None) => {}
        Err(e) => eprintln!("prov: could not remove about.md ({e})"),
    }
    Ok(())
}
