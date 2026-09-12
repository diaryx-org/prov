//! Identity and the lookups over it: `id`, `id --workspace`, `resolve`,
//! `backlinks`.
//!
//! `id` gives a document a stable handle within this workspace and
//! `id --workspace` gives the workspace a name among others; `resolve` and
//! `backlinks` are the two directions of the question the registry answers —
//! which file an id names, and which documents name this file.

use std::path::Path;
use std::process::ExitCode;

use prov::{Id, IdIndex, Trigger, Value, block_on, link};

use crate::CmdResult;
use crate::about::refresh_about;
use crate::config::write_config_setting;
use crate::session::{ensure_registry, entropy_seed, find_root, persist, workspace, ws_rel};

pub(crate) fn cmd_id(file: &Path) -> CmdResult {
    let mut ctx = find_root()?;
    if !ctx.config.identity.fires_on(Trigger::Link) {
        return Err("identity is off in this workspace's config \
             (run `prov config identity lazy` to enable stable IDs)"
            .into());
    }
    ensure_registry(&mut ctx)?;
    let mut ws = workspace(&ctx)?;
    let id = block_on(ws.register(&ws_rel(&ctx, file)?, Trigger::Link))?;
    persist(&ctx, &mut ws)?;
    println!("{}", link::id_target(&id));
    Ok(ExitCode::SUCCESS)
}

/// `prov id --workspace [NAME]` — ensure the *workspace* has a name, and print
/// it. The counterpart of [`cmd_id`] one level up: that gives a document an
/// identity within this workspace, this gives the workspace an identity among
/// others, so that `id:<name>/<id>` can point back here.
///
/// Deliberately **manual**. Nothing in prov mints a workspace name on its own,
/// and nothing needs one to work — an anonymous workspace is fully functional
/// and merely unaddressable from outside. The name is a commitment, since every
/// reference another archive writes is spelled with it, and prov does not make
/// commitments on the user's behalf. So it is minted here and nowhere else: this
/// command, or `prov init --workspace-id`, or a hand-written config key.
///
/// Also deliberately **idempotent, never a rename**. Re-running prints the name
/// already in config and writes nothing, even when a different NAME is passed —
/// because by then the old name is out in the world, in references this
/// workspace cannot see and could not fix. Renaming is available and stays
/// explicit: `prov config workspace_id <name>`.
///
/// Unlike [`cmd_id`] this does not consult `identity`: that axis decides whether
/// *documents* earn IDs, and a workspace can perfectly well be named while its
/// documents are addressed purely by path (a foreign reference into it would
/// then just carry that path's id-space, or nothing).
pub(crate) fn cmd_id_workspace(requested: Option<&str>) -> CmdResult {
    let mut ctx = find_root()?;
    let current = ctx.config.workspace_id.clone();
    if !current.is_empty() {
        if let Some(requested) = requested
            && requested != current
        {
            return Err(format!(
                "this workspace is already named `{current}` — references \
                 elsewhere are written with it, so renaming it to \
                 `{requested}` is `prov config workspace_id {requested}`"
            )
            .into());
        }
        eprintln!("already named (unchanged)");
        println!("{current}");
        return Ok(ExitCode::SUCCESS);
    }
    let name = match requested {
        Some(name) => {
            if !prov::is_valid_workspace_id(name) {
                return Err(format!(
                    "`{name}` is not a valid workspace name — it cannot be \
                     empty or contain `/`, `:` or whitespace (it has to survive \
                     being written as the qualifier of `id:<name>/<id>`)"
                )
                .into());
            }
            name.to_string()
        }
        // No name offered: mint an opaque global one. Wider than a document ID
        // by design — nothing can check a workspace name against the other
        // workspaces in the world, so width is the only uniqueness there is.
        None => prov::mint_workspace_id(entropy_seed()),
    };
    // Written as a *string* scalar rather than through `infer_scalar`: the mint
    // draws from an alphabet that includes the digits, so a name can look like a
    // number, and a `workspace_id: 123456789012` that read back as an integer
    // would be diagnosed malformed and ignored — the workspace would silently
    // stay anonymous right after being told it was named.
    let config_doc = write_config_setting(&mut ctx, "workspace_id", Value::String(name.clone()))?;
    eprintln!("named this workspace {name} in {}", config_doc.display());
    refresh_about(&ctx.root_dir)?;
    println!("{name}");
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_backlinks(file: &Path) -> CmdResult {
    let ctx = find_root()?;
    let target = ws_rel(&ctx, file)?;
    let links = block_on(workspace(&ctx)?.backlinks_to(&ctx.root_doc, &target))?;
    for backlink in &links {
        let kind = if backlink.by_id { "id" } else { "path" };
        println!("{}\t{}\t{kind}", backlink.source.display(), backlink.site);
    }
    if links.is_empty() {
        eprintln!("no backlinks to {}", target.display());
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_resolve(id: &str) -> CmdResult {
    let ctx = find_root()?;
    let ws = workspace(&ctx)?;
    let id = Id(id.strip_prefix(link::ID_SCHEME).unwrap_or(id).to_string());
    match ws.index().resolve(&id) {
        Some(path) => {
            println!("{}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        None if ws.index().is_tombstoned(&id) => {
            eprintln!("prov: {id} is tombstoned — its document was deleted");
            Ok(ExitCode::FAILURE)
        }
        None => {
            eprintln!("prov: {id} is not in this workspace's registry");
            Ok(ExitCode::FAILURE)
        }
    }
}
