//! The structural mutations: `new`, `attach`, `mv`, `reparent`, `rm`,
//! `restore`, `clear-deletions`, `duplicate`.
//!
//! Every verb here changes where a document sits in the containment tree, and
//! every one that may mint an ID ensures a registry exists *before* the
//! workspace is built over it — a route's synthesized nodes are `create`d and
//! mint on the same terms as the leaf. A parent is named the same way in all
//! of them ([`resolve_placement`]): a path, an `id:` handle, or an `@`-route
//! that `-p` may extend, so a route is only ever another way to name a
//! parent and never a different kind of operation.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use prov::{
    FileIndex, Layout, Mapping, Minter, RoutePlan, StdFs, Value, Workspace, block_on, edit, link,
};

use crate::about::refresh_about;
use crate::cli::{AttachArgs, NewArgs};
use crate::clock::now_rfc3339;
use crate::session::{Ctx, Session, TargetSpec, load, parse_target, resolve_target, ws_rel};
use crate::{AnyError, CmdResult};

/// Resolve a `--in DOC` / `--under ROUTE` placement to the parent document it
/// names. `Ok(None)` means the caller should stop having already printed
/// something the user asked for (a `--dry-run` preview).
///
/// Shared by `new`, `reparent`, and `mv`, because a route is only ever another
/// way to *name* a parent — never a different kind of operation. Extracted at the
/// third caller rather than the second: `new` alone justified nothing, but three
/// copies of the -p/--dry-run policy would drift, and the drift would be silent
/// (each command deciding on its own what a missing segment means).
///
/// Synthesized nodes are `create`d, so they mint IDs on the same terms as any
/// other document — a caller that mints must open the session with
/// [`Session::open_for_mutation`], so the registry exists *before* this runs
/// and not merely before its own write.
fn resolve_placement(
    session: &mut Session,
    target: &str,
    parents: bool,
    layout: Layout,
    dry_run: bool,
) -> Result<Option<PathBuf>, AnyError> {
    let route = match parse_target(target) {
        // A path or an id names a parent that must already exist — it has no
        // segments to synthesize. `-p` still applies to the *leaf* (idempotent
        // create, handled by the caller), so it is allowed here, just inert for
        // the parent.
        TargetSpec::Path(_) | TargetSpec::Id(_) => {
            let resolved = resolve_target(target)?;
            return Ok(Some(ws_rel(&session.ctx, &resolved)?));
        }
        TargetSpec::Route(route) => route,
    };
    let segments = Workspace::<StdFs>::route_segments(route);
    let plan = block_on(
        session
            .ws
            .plan_route(&session.ctx.root_doc, &segments, layout),
    )?;
    if dry_run {
        show_route_plan(route, &plan);
        if plan.is_complete() {
            eprintln!(
                "\nnothing to create; the route resolves to {}",
                plan.terminal.display()
            );
        } else if !parents {
            eprintln!(
                "\n{} segment(s) missing — re-run with -p to create them",
                plan.synthesize.len()
            );
        }
        return Ok(None);
    }
    if !plan.is_complete() && !parents {
        // Name the first missing segment and where the walk got to: the useful
        // half of the error is *how far* the route resolved.
        let missing = &plan.synthesize[0];
        return Err(format!(
            "@{route} stops at {}: no child titled {:?}\n\
             re-run with -p to create the missing segment(s), or --dry-run to preview",
            missing.parent.display(),
            missing.title,
        )
        .into());
    }
    let created = plan.synthesize.len();
    let terminal = block_on(session.ws.apply_route(&plan))?;
    // Synthesized route parents are incidental to the command's result (the leaf),
    // so their creation is narration → stderr; the caller's stdout is the terminal.
    for synth in &plan.synthesize {
        eprintln!("created {} ({:?})", synth.path.display(), synth.title);
    }
    if created > 0 {
        session.commit()?;
    }
    Ok(Some(terminal))
}

/// Print a route plan without applying it: what resolved, what is missing, and
/// where the missing nodes would land. Shared by `--dry-run` and the error a
/// missing route raises without `-p`, so the two describe the plan identically.
fn show_route_plan(route: &str, plan: &RoutePlan) {
    eprintln!("route {route:?}");
    for (depth, node) in plan.resolved.iter().enumerate() {
        eprintln!(
            "  {:indent$}{} (exists)",
            "",
            node.display(),
            indent = depth * 2
        );
    }
    let base = plan.resolved.len();
    for (depth, synth) in plan.synthesize.iter().enumerate() {
        eprintln!(
            "  {:indent$}{} (create, titled {:?})",
            "",
            synth.path.display(),
            synth.title,
            indent = (base + depth) * 2
        );
    }
}

/// Create a document under a parent named by `--in` — a path, an `id:` handle,
/// or an `@`-route through the containment tree (optionally with `-p` to create
/// the route segments that don't exist yet). The addressing mode is carried by
/// the value itself (see [`parse_target`]), not by a per-mode flag.
pub(crate) fn cmd_new(args: NewArgs) -> CmdResult {
    let NewArgs {
        title,
        in_target,
        parents,
        layout,
        dry_run,
        as_path,
        ext,
        set,
    } = args;
    let (title, in_target, layout) = (title.as_str(), in_target.as_str(), Layout::from(layout));
    let (as_path, ext, set) = (as_path.as_deref(), ext.as_deref(), set.as_slice());
    // Parsed before anything is written, so a malformed `--set` refuses the
    // command rather than leaving a document created without it.
    let sets = parse_sets(set)?;
    // Authoring a reference that registers mints IDs, as does an eager policy,
    // and a route's synthesized nodes mint on the same terms as the leaf — so
    // the mutating open, which bootstraps the registry first. A dry run writes
    // nothing and must not bootstrap one either.
    let mut session = if dry_run {
        Session::open()?
    } else {
        Session::open_for_mutation()?
    };

    // Resolve the parent. A path `--in` is already a path; a `@`-route walks the
    // tree from the root, and (with `-p`) creates what it doesn't find. Either way
    // the rest of this function is unchanged — a route is just another way to
    // *name* a parent, never a different kind of creation.
    let Some(parent_rel) = resolve_placement(&mut session, in_target, parents, layout, dry_run)?
    else {
        return Ok(ExitCode::SUCCESS);
    };

    // The new document's path: an explicit `--as` wins; otherwise a readable
    // filename derived from the title — `slug(title).<ext>` beside the parent,
    // where the extension is `--ext` or the workspace's content format. The title
    // itself is always recorded in metadata (structure lives there, not the name).
    let path = match as_path {
        Some(p) => ws_rel(&session.ctx, p)?,
        None => {
            let extension = ext
                .map(str::to_owned)
                .unwrap_or_else(|| session.ctx.config.content_format.extension().to_string());
            let name = format!("{}.{extension}", link::slug(title));
            parent_rel.parent().unwrap_or(Path::new("")).join(name)
        }
    };
    // Leaf idempotency (`-p`): a target that already exists as the *same*
    // document (same title) is a no-op — `mkdir -p` for the leaf, completing the
    // route-parent `-p` above, so a daily-note cron can re-run the same command.
    // A path held by a *different*-titled document is a real collision and still
    // errors. Without `-p`, an existing leaf errors as before (via `create`).
    if parents && session.ws.fs_path(&path).exists() {
        let (_, existing) = load(&session.ws.fs_path(&path))?;
        if existing.meta.get("title").and_then(Value::as_str) != Some(title) {
            return Err(format!(
                "{} already exists with a different title — refusing to reuse it \
                 (pick another title, or --as to name a different file)",
                path.display()
            )
            .into());
        }
        if dry_run {
            eprintln!(
                "exists: {} (in {}) — no-op",
                path.display(),
                parent_rel.display()
            );
            return Ok(ExitCode::SUCCESS);
        }
        // Ensure the containment link both ways (idempotent; refuses a contested
        // parent), so an existing-but-unlinked file converges too. The contract is
        // the *result*, not the action: an idempotent no-op still yields the path.
        block_on(session.ws.adopt(&path, &parent_rel))?;
        session.commit()?;
        eprintln!("exists: {} (in {})", path.display(), parent_rel.display());
        println!("{}", path.display());
        return Ok(ExitCode::SUCCESS);
    }
    if dry_run {
        eprintln!(
            "would create {} (in {})",
            path.display(),
            parent_rel.display()
        );
        return Ok(ExitCode::SUCCESS);
    }
    // (The session is the one opened above — reusing it keeps any IDs a route
    // just minted in the same in-memory index this create registers into.)
    let opening = opening_fields(&session.ctx, &session.ws, &parent_rel, sets)?;
    let created = block_on(
        session
            .ws
            .create_with_fields(&path, &parent_rel, title, &opening),
    )?;
    session.commit()?;
    // A separated child is a pair — the metadata node the parent links, plus its
    // prose body file. Name both in the narration so it is clear two files were
    // written; stdout carries only the node (the linkable document).
    match &created.body {
        Some(body) => {
            eprintln!(
                "created {} (in {})",
                created.node.display(),
                parent_rel.display()
            );
            eprintln!("  body: {}", body.display());
        }
        None => eprintln!(
            "created {} (in {})",
            created.node.display(),
            parent_rel.display()
        ),
    }
    println!("{}", created.node.display());
    Ok(ExitCode::SUCCESS)
}

/// The `--set KEY=VALUE` pairs, typed like `set`'s value. A pair without an
/// `=` is a refusal, not an empty value.
fn parse_sets(set: &[String]) -> Result<Vec<(String, Value)>, AnyError> {
    let mut out = Vec::with_capacity(set.len());
    for pair in set {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(format!("--set {pair}: expected KEY=VALUE").into());
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("--set {pair}: expected KEY=VALUE").into());
        }
        out.push((key.to_string(), edit::infer_scalar(value).into()));
    }
    Ok(out)
}

/// The fields a document made by `new` under `parent` opens with, beyond the
/// ones prov authors itself, in the order they are written: the workspace's
/// `created` stamp — the same clock and format as `updated`, and the library
/// is as clockless here as it is there — then each `fields.<name>.default`
/// whose declaration governs a child of `parent` (a `status: open` declared
/// under `Tasks` reaches a task and not the readme beside the index), then
/// every `--set`, which overrides a default of the same name.
///
/// Only the document `new` names gets these. The index nodes a route with
/// `-p` synthesizes on the way are made by the library's route walk, and a
/// `status: open` meant for a task is not meant for the month index above it.
///
/// Which declaration governs is found by climbing from `parent` to the root,
/// not by resolving every scope over the whole tree: `new` adds one document
/// under one parent, and should read as much of the workspace as that takes.
fn opening_fields(
    ctx: &Ctx,
    ws: &Workspace<StdFs, Minter, FileIndex>,
    parent: &Path,
    sets: Vec<(String, Value)>,
) -> Result<Mapping, AnyError> {
    let mut fields = Mapping::new();
    if !ctx.config.created.is_empty() {
        fields.insert(ctx.config.created.clone(), Value::String(now_rfc3339()));
    }
    for (name, value) in block_on(ws.defaults_for_child(&ctx.root_doc, &ctx.config, parent))? {
        fields.insert(name, value);
    }
    for (key, value) in sets {
        fields.insert(key, value);
    }
    Ok(fields)
}

/// Attach an arbitrary file — or, with `--all`, every loose file under the
/// workspace — minting a metadata sidecar and linking it under a parent
/// (default: the workspace root). Mirrors [`cmd_new`] — an id-registering
/// reference style or an eager policy mints IDs, so a registry is ensured first.
pub(crate) fn cmd_attach(args: AttachArgs) -> CmdResult {
    let AttachArgs {
        payload,
        in_target,
        parents,
        layout,
        opaque,
        all,
        recursive,
        manifest,
        no_hash,
    } = args;
    let (payload, in_target, layout) = (
        payload.as_deref(),
        in_target.as_deref(),
        Layout::from(layout),
    );
    let hash = !no_hash;
    if recursive && !all {
        return Err("--recursive only applies with --all".into());
    }
    let mut session = Session::open_for_mutation()?;
    // Default the parent to the workspace root — the common "attach this to my
    // workspace" case names no parent at all. Otherwise it is resolved exactly as
    // every other command resolves one, so an `@`-route `--in` works here too.
    let parent_rel = match in_target {
        None => session.ctx.root_doc.clone(),
        Some(t) => match resolve_placement(&mut session, t, parents, layout, false)? {
            Some(p) => p,
            None => return Ok(ExitCode::SUCCESS),
        },
    };

    if all {
        if payload.is_some() {
            return Err("pass a file or --all, not both".into());
        }
        // Bounded to reached directories by default; `--recursive` sweeps the
        // whole tree (a pure asset dump you know is all attachments).
        let loose = if recursive {
            block_on(session.ws.loose_attachments())?
        } else {
            block_on(session.ws.loose_attachments_in(&session.ctx.root_doc))?
        };
        if loose.is_empty() {
            eprintln!("no loose files to attach");
            return Ok(ExitCode::SUCCESS);
        }
        let mut attached = 0usize;
        for p in &loose {
            match block_on(session.ws.attach(p, &parent_rel)) {
                Ok(node) => {
                    eprintln!("attached {} (sidecar {})", p.display(), node.display());
                    println!("{}", node.display());
                    attached += 1;
                }
                Err(e) => eprintln!("prov: could not attach {}: {e}", p.display()),
            }
        }
        session.commit()?;
        eprintln!("attached {attached} file(s) under {}", parent_rel.display());
        return Ok(ExitCode::SUCCESS);
    }

    let Some(payload) = payload else {
        return Err("specify a file to attach, or pass --all".into());
    };
    let payload_rel = ws_rel(&session.ctx, payload)?;

    // The bulk form: the positional is a directory, and it gains one node and
    // one list rather than a sidecar per file.
    if manifest {
        let node = block_on(session.ws.attach_manifest_titled(
            &payload_rel,
            &parent_rel,
            None,
            hash,
        ))?;
        session.commit()?;
        let (manifest_doc, listed) = block_on(session.ws.manifest_of(&node))?
            .map(|(doc, m)| (doc, m.files.len()))
            .unwrap_or_default();
        eprintln!(
            "covered {} ({listed} file(s) in {}, node {} in {})",
            payload.display(),
            manifest_doc.display(),
            node.display(),
            parent_rel.display()
        );
        println!("{}", node.display());
        return Ok(ExitCode::SUCCESS);
    }

    let node = if opaque {
        block_on(session.ws.attach_opaque(&payload_rel, &parent_rel))?
    } else {
        block_on(session.ws.attach(&payload_rel, &parent_rel))?
    };
    session.commit()?;
    eprintln!(
        "attached {} (sidecar {} in {})",
        payload.display(),
        node.display(),
        parent_rel.display()
    );
    println!("{}", node.display());
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_mv(
    from: &str,
    to: &Path,
    in_target: Option<&str>,
    parents: bool,
    layout: Layout,
) -> CmdResult {
    let from_resolved = resolve_target(from)?;
    // `rename` mints nothing, but `--under -p` synthesizes nodes with `create`,
    // which does — so a registry has to exist before the route runs, exactly as in
    // `new`/`reparent`. Plain `mv` skips this and stays as cheap as it was.
    let mut session = if in_target.is_some() {
        Session::open_for_mutation()?
    } else {
        Session::open()?
    };
    let to_rel = ws_rel(&session.ctx, to)?;
    block_on(
        session
            .ws
            .rename(&ws_rel(&session.ctx, &from_resolved)?, &to_rel),
    )?;
    eprintln!("moved {} -> {}", from_resolved.display(), to.display());

    // The move first, then the reparent — in that order because `rename` has
    // already retargeted every inbound link, so the parent the reparent removes is
    // found at the document's *new* path. Doing it the other way would reparent a
    // path that is about to stop existing.
    if let Some(target) = in_target {
        let Some(parent_rel) = resolve_placement(&mut session, target, parents, layout, false)?
        else {
            return Ok(ExitCode::SUCCESS);
        };
        if block_on(session.ws.reparent(&to_rel, &parent_rel))? != prov::Reparented::Unchanged {
            eprintln!("reparented {} -> in {}", to.display(), parent_rel.display());
        }
    }
    session.commit()?;
    // The document's new location is the handle a caller acts on next.
    println!("{}", to_rel.display());
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_reparent(
    path: &str,
    in_target: &str,
    parents: bool,
    layout: Layout,
    dry_run: bool,
) -> CmdResult {
    // A route's synthesized nodes are `create`d and so mint on the same terms as
    // any other document — the registry must exist before the route is applied.
    // (The reparent itself authors links too, which an id-authoring workspace
    // registers.) A dry run writes nothing and must not bootstrap one either.
    let mut session = if dry_run {
        Session::open()?
    } else {
        Session::open_for_mutation()?
    };
    let Some(parent_rel) = resolve_placement(&mut session, in_target, parents, layout, dry_run)?
    else {
        return Ok(ExitCode::SUCCESS);
    };
    let path_rel = ws_rel(&session.ctx, &resolve_target(path)?)?;
    let outcome = block_on(session.ws.reparent(&path_rel, &parent_rel))?;
    session.commit()?;
    // Say which of the three happened. "reparented" for a run that wrote
    // nothing is how a workspace full of half-linked documents survives a
    // repair pass that reported success on every one of them.
    match outcome {
        prov::Reparented::Moved => eprintln!(
            "reparented {} -> in {}",
            path_rel.display(),
            parent_rel.display()
        ),
        prov::Reparented::Linked => eprintln!(
            "{} already claimed {} — added the missing entry in {}",
            path_rel.display(),
            parent_rel.display(),
            parent_rel.display()
        ),
        prov::Reparented::Unchanged => eprintln!(
            "{} is already in {}, both ways — nothing to do",
            path_rel.display(),
            parent_rel.display()
        ),
    }
    println!("{}", path_rel.display());
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_rm(path: &str, force: bool) -> CmdResult {
    let resolved = resolve_target(path)?;
    let mut session = Session::open()?;
    let target = ws_rel(&session.ctx, &resolved)?;

    // The `record_deletions` axis reaches the library through `Settings`, so the
    // one verb covers both postures; what the CLI adds is the clock, which the
    // library takes as an argument rather than reading.
    let recorded = session.ctx.config.record_deletions;
    let now = now_rfc3339();
    let danglers = block_on(session.ws.delete_with(
        &target,
        force,
        recorded.then_some(now.as_str()),
        prov::Diagnosis::Report,
    ))?;
    session.commit()?;
    if recorded {
        println!(
            "deleted {} (recorded; `prov restore` relinks it once the file is back)",
            resolved.display()
        );
    } else {
        println!("deleted {}", resolved.display());
    }
    // The first recorded delete *bootstraps* the log and adds the root's
    // `deletions` pointer — another machinery file the page lists. A no-op on
    // every later delete, since the pointer already exists.
    refresh_about(&session.ctx.root_dir)?;
    for finding in &danglers {
        eprintln!("warning: now dangling — {finding}");
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_restore(path: &str) -> CmdResult {
    let mut session = Session::open()?;
    // The path names a document that was deleted, so it cannot be
    // `resolve_target`-ed — that reads the file, and under the ordinary restore
    // the caller has only just put one back there. Take it as given, relative to
    // the workspace root.
    let from = ws_rel(&session.ctx, Path::new(path))?;
    block_on(session.ws.restore(&from, &session.ctx.root_doc))?;
    session.commit()?;
    eprintln!("restored {}", from.display());
    println!("{}", from.display());
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_clear_deletions() -> CmdResult {
    let mut session = Session::open()?;
    let forgotten = block_on(session.ws.clear_deletions(&session.ctx.root_doc))?;
    session.commit()?;
    // A bulk operation yields no object to name — narration only, stdout stays
    // empty.
    eprintln!("forgot {forgotten} deletion record(s)");
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn cmd_duplicate(source: &str) -> CmdResult {
    let resolved = resolve_target(source)?;
    // Attaching the copy authors the parent's spanning entry, which mints an ID
    // when that style registers (or under an eager policy) — same as `new`.
    let mut session = Session::open_for_mutation()?;
    let copy = block_on(session.ws.duplicate(&ws_rel(&session.ctx, &resolved)?))?;
    session.commit()?;
    eprintln!("duplicated {} -> {}", resolved.display(), copy.display());
    println!("{}", copy.display());
    Ok(ExitCode::SUCCESS)
}
