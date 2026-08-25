//! `prov` — command-line companion for the prov library.
//!
//! A thin adapter: parse arguments, call into the library, render the result.
//! The workspace *semantics* — discovery, bootstrap, the mutation engine — live
//! in `prov`; this crate is argument parsing, session plumbing, and
//! presentation.
//!
//! The crate is split across three modules to keep each legible:
//!
//! - [`cli`] — the `clap` argument grammar and the enums that mirror the
//!   library's config axes (the CLI *spelling* of each concept).
//! - [`init`] — the `init` command and its interactive intake (the one command
//!   that *creates* a workspace, and the largest).
//! - `main` (here) — the dispatcher, the session layer that discovers the
//!   workspace ([`find_root`]) and drives the library's [`prov::StdFs`]-backed
//!   engine through the dependency-free [`prov::block_on`] executor, and the
//!   remaining command handlers.
//!
//! Single-document commands (`show`, `links`, `meta`, `get`, `body`, `set`,
//! `unset`) operate on the pure layers and need no workspace; workspace commands
//! (`tree`, `check`, `new`, `mv`, `rm`, …) discover a root first.

use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use prov::document::MetaCarrier;
use prov::{
    Addressing, Adoption, ChangeSet, ContentFormat, ContentState, Document, EmbedStyle, FileIndex,
    Format, Id, IdIndex, IdStorage, IndexStore, Layout, LinkStyle, Mapping, Minter, Node, NodeKind,
    Notation, PathStyle, PeerResolver, RelationSet, RoutePlan, Settings, StdFs, StructurePlan,
    SynthNode, Target, Trigger, Value, Workspace, WorkspaceConfig, block_on, edit, link, meta,
};

mod backup;
mod cli;
mod json;
mod peer;
mod zip;
use cli::*;

fn main() -> ExitCode {
    let cli = Cli::parse();
    // `-C <dir>` / `--root <dir>` (or `PROV_ROOT`, which it overrides) runs prov
    // as if it had started in that directory: chdir once, up front, so every
    // downstream `current_dir()`-based root discovery and every relative path
    // argument resolves there — the `git -C` model, in one place.
    if let Some(dir) = cli
        .root
        .clone()
        .or_else(|| std::env::var_os("PROV_ROOT").map(PathBuf::from))
        && let Err(e) = std::env::set_current_dir(&dir)
    {
        eprintln!("prov: could not use root directory {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    // Resolved once, before any command runs, for the same reason `-C` is: it is
    // a property of the invocation, not of any one verb.
    peer::init(cli.peers.clone());
    let result = match cli.command {
        Command::Peer { action } => cmd_peer(action),
        Command::Show { file } => resolve_target(&file).and_then(|f| cmd_show(&f)),
        Command::Links { file, relation } => {
            resolve_target(&file).and_then(|f| cmd_links(&f, relation.as_deref()))
        }
        Command::Meta { file, format } => resolve_target(&file).and_then(|f| cmd_meta(&f, format)),
        Command::Get { file, key } => resolve_target(&file).and_then(|f| cmd_get(&f, &key)),
        Command::Body { file } => resolve_target(&file).and_then(|f| cmd_body(&f)),
        Command::Render { file } => resolve_target(&file).and_then(|f| cmd_render(&f)),
        Command::Init {
            dir,
            title,
            author,
            meta,
            embed,
            content,
            wrapper,
            reference,
            link_style,
            identity,
            id_storage,
            fixity,
            no_recycle_bin,
            updated_field,
            workspace_id,
            adopt,
            attach,
            yes,
        } => cmd_init(
            dir.as_deref(),
            title,
            author,
            meta,
            embed,
            content,
            wrapper,
            reference,
            link_style,
            identity,
            id_storage,
            fixity,
            no_recycle_bin,
            updated_field,
            workspace_id,
            adopt,
            attach,
            yes,
        ),
        Command::Edit { file } => resolve_target(&file).and_then(|f| cmd_edit(&f)),
        Command::Set { file, key, value } => {
            resolve_target(&file).and_then(|f| cmd_set(&f, &key, &value))
        }
        Command::Unset { file, key } => resolve_target(&file).and_then(|f| cmd_unset(&f, &key)),
        Command::Views { name } => cmd_views(name.as_deref()),
        Command::Exports { name } => cmd_exports(name.as_deref()),
        Command::Tree { root } => root
            .map(|r| resolve_target(&r))
            .transpose()
            .and_then(|r| cmd_tree(r.as_deref())),
        Command::Explore { file } => cmd_explore(file.as_deref()),
        Command::Check {
            root,
            fix,
            only,
            json,
        } => root.map(|r| resolve_target(&r)).transpose().and_then(|r| {
            let only = only.map(|o| resolve_target(&o)).transpose()?;
            cmd_check(r.as_deref(), fix, only.as_deref(), json)
        }),
        Command::Stamp {
            target,
            all,
            no_timestamp,
            dry_run,
        } => target
            .map(|t| resolve_target(&t))
            .transpose()
            .and_then(|t| cmd_stamp(t.as_deref(), all, no_timestamp, dry_run)),
        Command::New {
            title,
            in_target,
            parents,
            layout,
            dry_run,
            as_path,
            ext,
        } => cmd_new(
            &title,
            &in_target,
            parents,
            layout.into(),
            dry_run,
            as_path.as_deref(),
            ext.as_deref(),
        ),
        Command::Attach {
            payload,
            in_target,
            parents,
            layout,
            opaque,
            all,
            recursive,
            manifest,
            no_hash,
        } => cmd_attach(
            payload.as_deref(),
            in_target.as_deref(),
            parents,
            layout.into(),
            opaque,
            all,
            recursive,
            manifest,
            !no_hash,
        ),
        Command::Manifest {
            target,
            update,
            verify,
        } => cmd_manifest(&target, update, verify),
        Command::Mv {
            from,
            to,
            in_target,
            parents,
            layout,
        } => cmd_mv(&from, &to, in_target.as_deref(), parents, layout.into()),
        Command::Reparent {
            path,
            in_target,
            parents,
            layout,
            dry_run,
        } => cmd_reparent(&path, &in_target, parents, layout.into(), dry_run),
        Command::Rm { path, force, purge } => cmd_rm(&path, force, purge),
        Command::Restore { path } => cmd_restore(&path),
        Command::EmptyBin => cmd_empty_bin(),
        Command::Duplicate { source } => cmd_duplicate(&source),
        Command::Convert {
            file,
            axis,
            value,
            recursive,
            force,
        } => resolve_target(&file).and_then(|f| cmd_convert(&f, &axis, &value, recursive, force)),
        Command::Id { file, workspace } => match workspace {
            Some(name) => cmd_id_workspace(name.as_deref()),
            // `required_unless_present` makes this unreachable without the flag.
            None => resolve_target(&file.unwrap_or_default()).and_then(|f| cmd_id(&f)),
        },
        Command::Resolve { id } => cmd_resolve(&id),
        Command::Backlinks { file } => resolve_target(&file).and_then(|f| cmd_backlinks(&f)),
        Command::Config {
            key,
            value,
            setup,
            home,
        } => cmd_config(key.as_deref(), value.as_deref(), setup, home),
        Command::Backup { to, zip } => backup::cmd_backup(&to, zip),
        Command::About { check, print } => cmd_about(check, print),
        Command::Ignore { why, json } => cmd_ignore(why, json),
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("prov: {err}");
            ExitCode::FAILURE
        }
    }
}

type CmdResult = Result<ExitCode, Box<dyn std::error::Error>>;

/// The relation vocabulary. For now the diaryx preset; configurable vocabularies
/// (and a `--relations` flag) come later.
fn relation_set() -> RelationSet {
    RelationSet::diaryx()
}

/// The discovered workspace context: where the root is, which document is the
/// root, and where the root says the registry lives.
struct Ctx {
    /// Absolute path of the workspace root directory.
    root_dir: PathBuf,
    /// The root document, relative to `root_dir`.
    root_doc: PathBuf,
    /// The registry document the root declares (relative to `root_dir`), if any.
    registry: Option<PathBuf>,
    /// The effective workspace config (root frontmatter overlaid by the linked
    /// config document, over defaults).
    config: WorkspaceConfig,
}

type AnyError = Box<dyn std::error::Error>;

/// Resolve the workspace root and, on success, warn (once, to stderr) about any
/// config a command would otherwise run past silently — settings prov would
/// ignore, or a config `spec` newer than this build. Suppressed by
/// `PROV_QUIET`. Commands that already report config in full
/// (`check`, `config`) use [`find_root_quiet`] instead.
fn find_root() -> Result<Ctx, AnyError> {
    let ctx = find_root_quiet()?;
    warn_config(&ctx);
    Ok(ctx)
}

/// Warn about config that will not take effect — the proactive counterpart to
/// `check`'s [`prov::Finding::ConfigIssue`]. One stderr line summarizing
/// settings prov would silently ignore (a typo or unrecognized value across
/// either config surface), and one for a `spec` this build is too old to fully
/// read. Quiet when the config is clean, or when `PROV_QUIET` is set.
fn warn_config(ctx: &Ctx) {
    if std::env::var_os("PROV_QUIET").is_some() {
        return;
    }
    let mut issues = Vec::new();
    let mut spec_ahead = None;
    // The root's `prov:` block.
    if let Ok(text) = std::fs::read_to_string(ctx.root_dir.join(&ctx.root_doc))
        && let Ok(doc) = Document::parse(&ctx.root_doc, &text)
        && let Some(block) = doc.meta.get(prov::config::ROOT_CONFIG_KEY)
    {
        issues.extend(prov::diagnose(block));
        spec_ahead = spec_ahead.or_else(|| prov::spec_ahead(block));
    }
    // The dedicated config document.
    let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&ctx.root_dir).build();
    if let Ok(Some(config_doc)) = block_on(probe.config_path(&ctx.root_doc))
        && let Ok(text) = std::fs::read_to_string(ctx.root_dir.join(&config_doc))
        && let Ok(doc) = Document::parse(&config_doc, &text)
    {
        issues.extend(prov::diagnose(&doc.meta));
        spec_ahead = spec_ahead.or_else(|| prov::spec_ahead(&doc.meta));
    }
    if let Some(declared) = spec_ahead {
        eprintln!(
            "prov: config declares spec {declared} but this build understands spec {} — newer settings may be ignored (upgrade prov)",
            prov::config::SPEC_VERSION
        );
    }
    // "Not taking effect" rather than "ignored": most of these *are* keys prov
    // silently drops, but `views.<name>.nest` on a multi-valued field is read
    // and simply cannot be acted on. One summary line covers both; `check` says
    // which it is.
    if let Some(first) = issues.first() {
        eprintln!(
            "prov: {} config setting(s) will not take effect (e.g. `{}`) — run `prov check` for details",
            issues.len(),
            first.key
        );
    }
}

/// Find the workspace root by walking up from the current directory. The walk,
/// the root-candidate rule, and the tie-breaking all live in the library
/// ([`prov::discover`]); this only supplies the real current directory and
/// phrases the two failure modes as CLI diagnostics. Does not warn about config —
/// see [`find_root`].
fn find_root_quiet() -> Result<Ctx, AnyError> {
    let cwd = std::env::current_dir()?;
    find_root_quiet_at(&cwd)
}

/// [`find_root_quiet`], but discovering from `dir` rather than the process's
/// current directory — for a re-discovery after a write has changed the config
/// on disk, where the caller already knows the root.
fn find_root_quiet_at(dir: &Path) -> Result<Ctx, AnyError> {
    match block_on(prov::discover(&StdFs, dir))? {
        prov::Discovery::Found(d) => Ok(Ctx {
            root_dir: d.root_dir,
            root_doc: d.root_doc,
            registry: d.registry,
            config: d.config,
        }),
        prov::Discovery::Ambiguous { dir, candidates } => Err(format!(
            "ambiguous workspace root in {}: {} (rename one, or add part_of)",
            dir.display(),
            candidates.join(", ")
        )
        .into()),
        prov::Discovery::NotFound => Err(
            "no workspace root found: no ancestor directory has a document \
with metadata and no part_of\n\
\n\
  If this directory holds content already, run `prov init` here to adopt it\n\
  (use `prov init --adopt` to link existing files in non-interactively).\n\
  Otherwise `prov init` starts a fresh workspace."
                .into(),
        ),
    }
}

/// The workspace the multi-document commands drive: rooted at the discovered
/// root, a lazy identity policy, and the registry the root declares (an empty
/// in-memory one when the root declares none — see `ensure_registry`).
fn workspace(ctx: &Ctx) -> Result<Workspace<StdFs, Minter, FileIndex>, AnyError> {
    let index = if ctx.config.id_storage == IdStorage::FrontmatterOnly {
        // No registry document: rebuild the id→path map by scanning each file's
        // self-stored `id` field — a flat scan, independent of link resolution.
        let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&ctx.root_dir).build();
        let mut index = FileIndex::new(ctx.config.default_embed_format);
        for (id, path) in block_on(probe.scan_ids())? {
            index.register(&id, &path);
        }
        // A scanned index reflects on-disk state, so it starts clean.
        index.mark_clean();
        index
    } else {
        match &ctx.registry {
            Some(rel) => {
                let full = ctx.root_dir.join(rel);
                let text = match std::fs::read_to_string(&full) {
                    Ok(text) => text,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => return Err(e.into()),
                };
                FileIndex::parse(rel, &text)?
            }
            // No registry declared yet: an empty in-memory one in the workspace's
            // metadata format, so a later bootstrap writes that format.
            None => FileIndex::new(ctx.config.default_embed_format),
        }
    };
    // Every policy knob comes from the config, whole: the relation vocabulary
    // (declared definitions + spanning, or the diaryx preset, with per-relation
    // `style` overrides overlaid), the reference style, the embedding pair the
    // store's own documents are authored through, the fixity and history axes,
    // the identity-storage mode, and what this workspace calls itself. Threading
    // them one at a time is what `Settings` exists to stop; a knob added to the
    // config now reaches the workspace without touching this function.
    //
    // The one thing that cannot come across is `identity`: it is a policy *type*
    // here, not a value, which is what lets identity be compiled out entirely.
    Ok(Workspace::builder(StdFs)
        .root(&ctx.root_dir)
        .settings(Settings::from(&ctx.config))
        .identity(Minter::with(ctx.config.identity, entropy_seed()))
        .index(index)
        .build())
}

/// Make sure the workspace *declares* a registry, bootstrapping one when it
/// does not: create `registry.<ext>` (in the workspace's metadata format) beside
/// the root (self-described with a title and a part_of back to the root) and add
/// the `registry` pointer to the root's metadata — comment-preservingly, like
/// any other edit.
///
/// Two files, so one [`ChangeSet`]: a bootstrap that wrote the registry document
/// but failed to point the root at it would leave a registry no scan can find —
/// invisible, and silently re-bootstrapped (over) next run.
fn ensure_registry(ctx: &mut Ctx) -> Result<(), AnyError> {
    // Frontmatter-only storage keeps no registry document — IDs live solely in
    // each file's `id` field, so there is nothing to bootstrap or point at.
    if !ctx.config.id_storage.keeps_registry() {
        return Ok(());
    }
    if ctx.registry.is_some() {
        return Ok(());
    }
    let format = ctx.config.default_embed_format;
    let registry_rel = PathBuf::from(sidecar_name(REGISTRY_STEM, format));

    // Seed: a self-describing node titled "ID registry". Machinery is reached
    // *one-way* through the root's `registry` pointer, so it carries no `part_of`
    // back-link — that would assert a spanning-tree membership it does not have
    // (DESIGN §5, "link target kinds"). The crash-safe "create sidecar + point the
    // root at it" landing lives in the library ([`Workspace::link_sidecar`]).
    let mut seed = Mapping::new();
    seed.insert("title".into(), Value::String("ID registry".into()));
    let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&ctx.root_dir).build();
    let created =
        block_on(probe.link_sidecar(&ctx.root_doc, "registry", &registry_rel, &seed, format))?;
    if created {
        eprintln!(
            "initialized {} (linked from {})",
            registry_rel.display(),
            ctx.root_doc.display()
        );
        // A new machinery file the root now points at, which `about.md` lists
        // among the files the spine will never reach. Bootstrapping one is a
        // change to the workspace's declared structure, not to its contents, so
        // it is squarely inside what the page describes.
        refresh_about(&ctx.root_dir)?;
    }
    ctx.registry = Some(registry_rel);
    Ok(())
}

/// Persist the registry when a mutation could not stage it itself.
///
/// Normally this does nothing: the library stages the registry write into the
/// same change set as the documents whose links it describes, so by the time a
/// command returns, the index is already clean. The exception is a workspace
/// with no registry document *yet* — `check --fix` deliberately declines to
/// bootstrap one until a fix has actually minted an ID, so the index it dirtied
/// had nowhere to stage to. Give it its new home and write it.
fn save_index(ctx: &Ctx, ws: &mut Workspace<StdFs, Minter, FileIndex>) -> Result<(), AnyError> {
    if !ws.index().is_dirty() {
        return Ok(());
    }
    let Some(rel) = &ctx.registry else {
        return Err("the registry changed but no registry document is declared".into());
    };
    let full = ctx.root_dir.join(rel);
    let host_text = match std::fs::read_to_string(&full) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    ws.index_mut().set_host(rel, &host_text)?;
    let Some((path, rendered)) = ws.index_mut().pending_write()? else {
        return Ok(());
    };
    let mut cs = ChangeSet::new();
    cs.write(path, rendered);
    block_on(prov::journal::workspace_journal().apply(&cs, &StdFs, &ctx.root_dir))?;
    ws.index_mut().committed(true);
    Ok(())
}

/// Persist a mutation's identity changes according to the workspace's
/// [`IdStorage`] mode: stamp each live ID into its document's `id` frontmatter
/// (frontmatter / frontmatter-only), and write the registry snapshot (registry /
/// frontmatter). Frontmatter-only keeps no registry, so the in-memory index —
/// rebuilt next run by scanning — is simply marked clean.
fn persist(ctx: &Ctx, ws: &mut Workspace<StdFs, Minter, FileIndex>) -> Result<(), AnyError> {
    if ctx.config.id_storage.stamps_frontmatter() {
        stamp_ids(ctx, ws)?;
    }
    if ctx.config.id_storage.keeps_registry() {
        save_index(ctx, ws)?;
    } else {
        // No registry document to write; the id→path map is derived from the
        // frontmatter we just stamped, so discard the dirtiness.
        ws.index_mut().mark_clean();
    }
    Ok(())
}

/// Stamp every live ID into its document's `id` frontmatter field, so the ID
/// travels with the file (DESIGN §5's self-describing shadow). Idempotent: a
/// document already carrying the right ID is left untouched, so this both
/// back-fills a workspace that just switched to frontmatter storage and records
/// freshly-minted IDs. A tombstoned ID has no live path and is skipped.
fn stamp_ids(ctx: &Ctx, ws: &mut Workspace<StdFs, Minter, FileIndex>) -> Result<(), AnyError> {
    let pairs: Vec<(Id, PathBuf)> = ws
        .index()
        .iter()
        .map(|(id, path)| (id.clone(), path.clone()))
        .collect();
    for (id, rel) in pairs {
        let full = ctx.root_dir.join(&rel);
        let Ok(text) = std::fs::read_to_string(&full) else {
            continue;
        };
        let Ok(doc) = Document::parse(&rel, &text) else {
            continue;
        };
        // Already carries this exact ID — nothing to write.
        if doc.meta.get("id").and_then(Value::as_str) == Some(id.0.as_str()) {
            continue;
        }
        // Always a string scalar, never `infer_scalar`: an ID from the NOID
        // alphabet may be all digits, and inferring would stamp it as an integer
        // (dropping any leading zero) that `Value::as_str` then can't read back.
        let updated = edit::set_in_text(
            &text,
            doc.carrier,
            "id",
            (&Value::String(id.0.clone())).into(),
        )?;
        std::fs::write(&full, updated)?;
    }
    Ok(())
}

/// How a CLI argument names a document — the addressing mode carried by the
/// *value*, not by which flag it was passed to.
///
/// This mirrors the library's [`Addressing`](prov::Addressing) (`Path`/`Id`/
/// `Alias`) and its `Link::parse`, which have always disambiguated a target by its
/// own syntax. The CLI briefly did it with flag names instead (`--in-path` vs
/// `--in-title`), which cost a flag per mode per argument and could only ever be
/// afforded on *one* argument — the parent — leaving every subject path-only. A
/// grammar costs one flag total and works in every slot, including subjects.
///
/// The spellings are chosen so a bare path stays a bare path: `id:` is the
/// library's own [`ID_SCHEME`](prov::link::ID_SCHEME), and `@` is not legal at
/// the start of a *relative* path anyone writes by habit. A file genuinely named
/// `@foo.md` is still addressable as `./@foo.md`, which parses as a path.
#[derive(Debug, PartialEq, Eq)]
enum TargetSpec<'a> {
    /// A filesystem path — the default, and the only mode that needs no workspace.
    Path(&'a str),
    /// `id:<id>` (or the legacy `prov:<id>`) — resolved through the registry.
    Id(&'a str),
    /// `@Daily/2026/08` — a route of titles walked from the workspace root. Bare
    /// `@` is the root document itself.
    Route(&'a str),
}

/// Classify a CLI target. Pure text: no filesystem, no workspace, no guessing —
/// the string says which mode it is or it is a path.
fn parse_target(s: &str) -> TargetSpec<'_> {
    if let Some(id) = link::strip_id_scheme(s) {
        return TargetSpec::Id(id);
    }
    match s.strip_prefix('@') {
        Some(route) => TargetSpec::Route(route),
        None => TargetSpec::Path(s),
    }
}

/// Resolve a target that names an *existing* document, to a path this process can
/// open (absolute for id/route, as-written for a path).
///
/// Root discovery is **lazy**: a plain path resolves without one, so `show`,
/// `meta`, `get`, `body`, `links`, `render`, `set`, and `unset` keep working on any
/// file anywhere — outside a workspace, in a tarball, wherever. Only `@` and `id:`
/// need a workspace, and only then is one discovered. That property is worth
/// keeping: those commands read a *file*, and only the other modes make the
/// argument mean a *node*.
fn resolve_target(s: &str) -> Result<PathBuf, AnyError> {
    match parse_target(s) {
        TargetSpec::Path(p) => Ok(PathBuf::from(p)),
        TargetSpec::Id(id) => {
            let ctx = find_root()?;
            let ws = workspace(&ctx)?;
            let id = Id(id.to_string());
            match ws.index().resolve(&id) {
                Some(path) => Ok(ctx.root_dir.join(path)),
                None if ws.index().is_tombstoned(&id) => {
                    Err(format!("{id} is tombstoned — its document was deleted").into())
                }
                None => Err(format!("{id} is not in the registry").into()),
            }
        }
        TargetSpec::Route(route) => {
            let ctx = find_root()?;
            let ws = workspace(&ctx)?;
            let terminal = resolve_route(&ctx, &ws, route)?;
            Ok(ctx.root_dir.join(terminal))
        }
    }
}

/// Walk a route of titles to an existing node, workspace-relative. Refuses to
/// create: a *subject* that does not exist is a mistake, never an instruction —
/// only a `--in` destination may be synthesized, and only with `-p`.
fn resolve_route(
    ctx: &Ctx,
    ws: &Workspace<StdFs, Minter, FileIndex>,
    route: &str,
) -> Result<PathBuf, AnyError> {
    let segments = Workspace::<StdFs>::route_segments(route);
    let plan = block_on(ws.plan_route(&ctx.root_doc, &segments, Layout::Nested))?;
    if !plan.is_complete() {
        let missing = &plan.synthesize[0];
        return Err(format!(
            "@{route} stops at {}: no child titled {:?}",
            missing.parent.display(),
            missing.title,
        )
        .into());
    }
    Ok(plan.terminal)
}

/// Re-anchor a (cwd-relative) CLI path to the discovered workspace root.
fn ws_rel(ctx: &Ctx, path: &Path) -> Result<PathBuf, AnyError> {
    let abs = link::normalize(std::env::current_dir()?.join(path));
    abs.strip_prefix(&ctx.root_dir)
        .map(Path::to_path_buf)
        .map_err(|_| {
            format!(
                "{} is outside the workspace root {}",
                path.display(),
                ctx.root_dir.display()
            )
            .into()
        })
}

/// A seed for the minter from OS-seeded hasher state — dependency-free
/// randomness. (Uniqueness is enforced by rejection against the registry;
/// the seed only needs to differ between runs.)
fn entropy_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::hash::RandomState::new().build_hasher().finish()
}

fn load(file: &Path) -> Result<(String, Document), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(file)?;
    let doc = Document::parse(file, &text)?;
    Ok((text, doc))
}

mod init;
use init::cmd_init;

fn cmd_show(file: &Path) -> CmdResult {
    let (_, doc) = load(file)?;
    let set = relation_set();

    println!("{}", file.display());

    if let Some(title) = doc.meta.get("title").and_then(Value::as_str) {
        println!("  title: {title}");
    }

    if !doc.has_meta() {
        println!("  (no embedded metadata)");
        return Ok(ExitCode::SUCCESS);
    }

    let children = set.children(&fig::Value::from(&doc.meta));
    if let Some(spanning) = set.spanning_relation() {
        println!("  {spanning} ({} children):", children.len());
        for child in &children {
            println!("    - {child}");
        }
    }

    // Overlay relations (everything that isn't the spanning tree), grouped and
    // printed in the vocabulary's declared order.
    let spanning = set.spanning_relation();
    let edges = set.edges(&fig::Value::from(&doc.meta));
    for relation in set.relations() {
        if Some(relation.name.as_str()) == spanning {
            continue;
        }
        let targets: Vec<&str> = edges
            .iter()
            .filter(|e| e.relation == relation.name)
            .map(|e| e.target.as_str())
            .collect();
        if targets.is_empty() {
            continue;
        }
        println!("  {}:", relation.name);
        for target in targets {
            println!("    - {target}");
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn cmd_links(file: &Path, relation: Option<&str>) -> CmdResult {
    let (_, doc) = load(file)?;
    for edge in relation_set().edges(&fig::Value::from(&doc.meta)) {
        if relation.is_none_or(|want| want == edge.relation) {
            println!("{}\t{}", edge.relation, edge.target);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_meta(file: &Path, format: Option<MetaFormat>) -> CmdResult {
    let (_, doc) = load(file)?;
    let Some(mapping) = doc.meta.as_mapping() else {
        return Err("document has no embedded metadata".into());
    };
    // Default to the format the document already uses.
    let format = format
        .map(Format::from)
        .unwrap_or_else(|| doc.carrier.map(|c| c.format()).unwrap_or(Format::Yaml));
    print!("{}", meta::serialize_mapping(mapping, format)?);
    Ok(ExitCode::SUCCESS)
}

fn cmd_get(file: &Path, key: &str) -> CmdResult {
    let (_, doc) = load(file)?;
    let mut value = &doc.meta;
    for part in key.split('.') {
        value = match part.parse::<usize>() {
            Ok(index) => value.as_sequence().and_then(|s| s.get(index)),
            Err(_) => value.get(part),
        }
        .ok_or_else(|| format!("no `{key}` in {}", file.display()))?;
    }
    match value {
        Value::Null => println!("null"),
        Value::Bool(b) => println!("{b}"),
        Value::Int(i) => println!("{i}"),
        Value::Float(f) => println!("{f}"),
        Value::String(s) => println!("{s}"),
        compound => {
            let format = doc.carrier.map(|c| c.format()).unwrap_or(Format::Yaml);
            print!("{}", meta::serialize_value(compound, format)?);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// A document's prose and the file declaring its grammar — [`prov::Body`] for a
/// caller that reads by real path rather than through a workspace.
///
/// `body`/`render` name a file directly and never open a workspace, so the
/// `content` target is resolved against the file's own directory here rather
/// than through `Graph::body`. A *separated* document keeps its prose in that
/// sibling; asking its `.yaml` node for `doc.body` gets the empty string, which
/// reads as "this document has no prose" and is not what happened.
fn body_of(file: &Path) -> Result<(String, PathBuf), Box<dyn std::error::Error>> {
    let (_, doc) = load(file)?;
    let Some(content) = doc.content_path(file) else {
        return Ok((doc.body, file.to_path_buf()));
    };
    if doc.is_attachment() {
        return Err(format!(
            "{}: attachment sidecar for {} — an opaque payload, not a prose body",
            file.display(),
            content.display(),
        )
        .into());
    }
    Ok((std::fs::read_to_string(&content)?, content))
}

fn cmd_body(file: &Path) -> CmdResult {
    let (text, _) = body_of(file)?;
    print!("{text}");
    Ok(ExitCode::SUCCESS)
}

fn cmd_render(file: &Path) -> CmdResult {
    let (text, from) = body_of(file)?;
    let format = prov::ContentFormat::from_extension(&from).ok_or_else(|| {
        format!(
            "{}: not a recognized body format (expected .md/.markdown or .dj/.djot)",
            from.display()
        )
    })?;
    let html = prov::render_html(&text, format)?;
    print!("{html}");
    Ok(ExitCode::SUCCESS)
}

fn cmd_set(file: &Path, key: &str, value: &str) -> CmdResult {
    let (text, doc) = load(file)?;
    let updated = edit::set_in_text(&text, doc.carrier, key, edit::infer_scalar(value))?;
    std::fs::write(file, updated)?;
    println!("{}", file.display());
    Ok(ExitCode::SUCCESS)
}

fn cmd_unset(file: &Path, key: &str) -> CmdResult {
    let (text, doc) = load(file)?;
    let updated = edit::unset_in_text(&text, doc.carrier, key)?;
    std::fs::write(file, updated)?;
    println!("{}", file.display());
    Ok(ExitCode::SUCCESS)
}

fn cmd_edit(file: &Path) -> CmdResult {
    // Snapshot before the editor so we can tell whether the user actually changed
    // anything — an open-and-quit must not bump the timestamp or restamp.
    let before = std::fs::read(file).ok();
    edit_file(file)?;
    let changed = std::fs::read(file).ok() != before;

    let ctx = find_root()?;
    let rel = ws_rel(&ctx, file)?;
    if !changed {
        eprintln!("edited {} (no changes)", rel.display());
        println!("{}", rel.display());
        return Ok(ExitCode::SUCCESS);
    }

    // The bookkeeping a real edit implies, in one crash-safe write: restamp the
    // body checksum (under `full`), and stamp the `updated` field (when
    // configured) with the current time — RFC 3339 UTC, the machine-standard
    // value the library reads back (DESIGN §2). Both self-gate, so this is a
    // no-op when neither is enabled.
    let mut ws = workspace(&ctx)?;
    let now = now_rfc3339();
    let updated =
        (!ctx.config.updated.is_empty()).then_some((ctx.config.updated.as_str(), now.as_str()));
    let wrote = block_on(ws.record_content_update(&rel, updated))?;
    persist(&ctx, &mut ws)?;

    match (wrote, updated.is_some()) {
        (true, true) => eprintln!(
            "edited {} — stamped `{}` + checksum",
            rel.display(),
            ctx.config.updated
        ),
        (true, false) => eprintln!("edited {} — content checksum updated", rel.display()),
        (false, true) => eprintln!(
            "edited {} — stamped `{}`",
            rel.display(),
            ctx.config.updated
        ),
        _ => eprintln!("edited {}", rel.display()),
    }
    println!("{}", rel.display());
    Ok(ExitCode::SUCCESS)
}

/// `stamp` — the bookkeeping of an edit prov did not host.
///
/// [`cmd_edit`] already does this for an edit it launched the editor for, and
/// it can be unconditional about the timestamp because it snapshotted the bytes
/// before handing over. Nothing here saw the edit happen, so the checksum is
/// the only evidence available, and [`ContentState`] is how that evidence is
/// read:
///
/// - **`Drifted`** — the bytes changed. Both stamps land, in the single
///   crash-safe write `record_content_update` makes of them.
/// - **`Intact`** — the bytes did not change. Nothing is written, which is what
///   makes re-running this free and puts it safely in a sync hook.
/// - **`Unrecorded`** — no checksum on record (fixity off, or a document that
///   predates it). There is no evidence either way, so a *named* target is
///   stamped on the strength of the user having named it, and `--all` skips it:
///   naming one file asserts an edit, sweeping a workspace does not.
/// - **`Unverifiable`** — a digest from an algorithm this build cannot compute.
///   Skipped, never overwritten, exactly as `check` leaves it alone.
///
/// Narration to stderr; stdout carries the machine value, one stamped path per
/// line — so `prov stamp --all | xargs …` gets what actually changed.
fn cmd_stamp(target: Option<&Path>, all: bool, no_timestamp: bool, dry_run: bool) -> CmdResult {
    let ctx = find_root()?;
    let mut ws = workspace(&ctx)?;

    // What to consider, and whether a name was put to each one.
    //
    // `--all` takes the document population `check` validates, not the *file*
    // set: a shadowed payload (`attach --opaque`) is bytes prov holds without
    // interpreting, and any `content_hash` inside one belongs to the exhibit
    // rather than to this workspace. An ordinary attachment payload is still in
    // here and still will not parse as a document — it is covered through its
    // sidecar, and skipped below where it is found.
    let targets: Vec<PathBuf> = match (target, all) {
        (Some(path), _) => vec![ws_rel(&ctx, path)?],
        (None, true) => block_on(ws.reachable_documents_from(&ctx.root_doc))?
            .into_iter()
            .collect(),
        (None, false) => {
            return Err("nothing to stamp: name a document, or pass --all".into());
        }
    };
    let named = target.is_some();

    let now = now_rfc3339();
    let field = &ctx.config.updated;
    // The workspace may not record an `updated` field at all, in which case
    // there is no timestamp half to this command and only the checksum moves.
    let timestamp = (!no_timestamp && !field.is_empty()).then_some((field.as_str(), now.as_str()));

    let mut stamped = 0usize;
    let mut seeded = 0usize;
    let mut skipped = 0usize;
    for path in targets {
        let state = match block_on(ws.content_state(&path)) {
            Ok(state) => state,
            // Under `--all` this is a reached file that is not a document (an
            // attachment's payload, a manifest's covered bytes) — not an error,
            // just not this command's business. A named target that cannot be
            // read is.
            Err(_) if !named => continue,
            Err(e) => return Err(format!("{}: {e}", path.display()).into()),
        };
        // Which stamps this document has earned. The two halves are decided
        // separately because they rest on different evidence: a checksum
        // *restates* the bytes, so it is owed wherever it is missing or wrong,
        // while a timestamp *asserts* that an edit happened, which only drift
        // or the user naming the file can establish.
        //
        // That split is what makes `--all` worth running: it brings a whole
        // workspace's fixity up to date — seeding the documents that never had
        // a checksum, correcting the ones that drifted — and claims an edit
        // time for exactly the drifted ones, never for a document it merely
        // read.
        let (write, claims_edit) = match state {
            ContentState::Drifted => (true, true),
            ContentState::Unrecorded => (true, named),
            ContentState::Intact | ContentState::Unverifiable => (false, false),
        };
        let timestamp = claims_edit.then_some(timestamp).flatten();
        if !write {
            if named {
                eprintln!(
                    "{}: {} — nothing to stamp",
                    path.display(),
                    match state {
                        ContentState::Intact => "checksum still matches the bytes",
                        ContentState::Unverifiable =>
                            "checksum uses an algorithm this build cannot compute",
                        _ => "unchanged",
                    }
                );
            }
            skipped += 1;
            continue;
        }
        if dry_run {
            // Both states that reach here are being stamped *for* the
            // checksum, so that is what a dry run names. Whether one actually
            // lands on an unrecorded document depends on the workspace's fixity
            // tier covering its kind, which only the write answers — and which
            // "would" already leaves open.
            eprintln!(
                "{}: would stamp {}",
                path.display(),
                stamp_summary(&ctx, timestamp, true)
            );
            println!("{}", path.display());
            if state == ContentState::Unrecorded {
                seeded += 1;
            } else {
                stamped += 1;
            }
            continue;
        }
        if block_on(ws.record_content_update(&path, timestamp))? {
            // Whether a checksum actually landed is not knowable up front for an
            // `Unrecorded` document: `record_content_update` writes one only if
            // the workspace's fixity tier covers this document's *kind*, which
            // is a decision the library makes and does not report. Re-reading
            // the state is how the narration stays a claim about what happened
            // rather than about what was attempted — one extra read, and only
            // for a document that was actually written.
            let hashed = match state {
                ContentState::Drifted => true,
                _ => block_on(ws.content_state(&path))? == ContentState::Intact,
            };
            eprintln!(
                "{}: stamped {}",
                path.display(),
                stamp_summary(&ctx, timestamp, hashed)
            );
            println!("{}", path.display());
            if state == ContentState::Unrecorded {
                seeded += 1;
            } else {
                stamped += 1;
            }
        } else {
            // `record_content_update` self-gates on the same two questions, so
            // it can decline what `content_state` waved through — a workspace
            // whose fixity tier does not cover this document's kind, with no
            // `updated` field configured either. Nothing to write, and nothing
            // wrong.
            if named {
                eprintln!(
                    "{}: this workspace records neither a checksum nor a timestamp for it",
                    path.display()
                );
            }
            skipped += 1;
        }
    }

    if all {
        let verb = if dry_run { "would stamp" } else { "stamped" };
        eprintln!(
            "{verb} {stamped} drifted document(s), seeded {seeded} that had no checksum; \
{skipped} unchanged"
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Which stamps a write landed, for one narration line.
fn stamp_summary(ctx: &Ctx, timestamp: Option<(&str, &str)>, hashed: bool) -> String {
    match (hashed, timestamp) {
        (true, Some(_)) => format!("`{}` + checksum", ctx.config.updated),
        (true, None) => "checksum".into(),
        (false, Some(_)) => format!("`{}`", ctx.config.updated),
        // `record_content_update` reported a write, so something moved; the
        // only remaining possibility is a timestamp field this narration was
        // not given. Unreachable in practice, and not worth a panic.
        (false, None) => "nothing".into(),
    }
}

/// The current time as an RFC 3339 UTC timestamp with microsecond precision
/// (`2026-07-16T14:30:00.123456Z`) — the machine-standard value prov stores for
/// provenance fields like `updated` and a history event's `created` (DESIGN §2:
/// prov-maintained ⟹ prov owns the format, and human-friendly rendering is a
/// viewer's job).
///
/// **This is the workspace's only clock.** Every timestamp prov writes comes from
/// here, which is what keeps one decision about precision from having to be
/// remembered at each site.
///
/// Hand-rolled from the system clock rather than pulling in a date crate, in the
/// spirit of the dependency-free SHA-256 and journal checksum. A pre-epoch clock
/// (only a badly-wrong system) formats as the epoch.
fn now_rfc3339() -> String {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    rfc3339(since.as_secs(), since.subsec_micros())
}

/// Format an instant since the Unix epoch as an RFC 3339 UTC timestamp. Split out
/// from [`now_rfc3339`] so the calendar arithmetic is testable without a clock.
///
/// The fraction is **always six digits, never trimmed**. Sub-second precision
/// exists so that two events in the same second can be *ordered*, and a variable
/// number of digits would defeat exactly that: `…10.1Z` against `…10.12Z`
/// compares `Z` (0x5A) with `2` (0x32) at the second fraction digit, so the
/// shorter one sorts later. Fixed width keeps a plain string comparison a correct
/// total order. (Timestamps written before this precision existed carry no
/// fraction at all; readers normalize — see `prov::history`.)
fn rfc3339(secs: u64, micros: u32) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{micros:06}Z")
}

/// Days since 1970-01-01 → (year, month, day), by Howard Hinnant's civil-calendar
/// algorithm — exact for the whole proleptic Gregorian range, no leap-year
/// special-casing beyond the era arithmetic.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64; // day-of-era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { y + 1 } else { y }, month, day)
}

/// `prov views [NAME]` — list the declared views, or execute one.
///
/// Listing is deliberately the no-argument form. A view is a thing a workspace
/// *says about itself*, and the first question about one is whether the
/// workspace agrees it exists — which is also the fastest way to find out that
/// a `views:` block went unread because a key was misspelled (the stderr
/// warning `find_root` already prints covers the why).
fn cmd_views(name: Option<&str>) -> CmdResult {
    let ctx = find_root()?;
    let views = &ctx.config.views;
    let Some(name) = name else {
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
    let selection = block_on(prov::views::select(ws.graph(), view, &ctx.root_doc))?;
    let rows = prov::views::group(&selection, &view.group);
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
fn cmd_exports(name: Option<&str>) -> CmdResult {
    let ctx = find_root()?;
    let exports = &ctx.config.exports;
    let Some(name) = name else {
        if exports.is_empty() {
            println!("this workspace declares no exports");
            return Ok(ExitCode::SUCCESS);
        }
        for export in exports {
            let view = match &export.view {
                Some(view) => format!(", arranged by {view}"),
                None => String::new(),
            };
            println!(
                "{}  {} — gate: {}: {}{view}",
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
        "\n{} document(s) leave, {} held back by the gate{}",
        plan.entries.len(),
        plan.withheld.len(),
        match plan.outside_view.len() {
            0 => String::new(),
            n => format!(", {n} outside the view"),
        }
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_tree(root: Option<&Path>) -> CmdResult {
    let ctx = find_root()?;
    let root = match root {
        Some(r) => ws_rel(&ctx, r)?,
        None => ctx.root_doc.clone(),
    };
    let node = block_on(workspace(&ctx)?.tree(&root))?;
    print_node(&node, "", true, true);
    Ok(ExitCode::SUCCESS)
}

/// Render one tree node: `path — title (marker)`, then its children with
/// box-drawing connectors.
fn print_node(node: &Node, prefix: &str, is_last: bool, is_root: bool) {
    let connector = if is_root {
        String::new()
    } else {
        format!("{prefix}{}", if is_last { "└── " } else { "├── " })
    };
    let name = node
        .title
        .as_deref()
        .or(node.label.as_deref())
        .map(|t| format!("{} — {t}", node.path.display()))
        .unwrap_or_else(|| node.path.display().to_string());
    let marker = match &node.kind {
        NodeKind::Doc => String::new(),
        NodeKind::Missing => " (missing)".to_string(),
        NodeKind::Cycle => " (cycle!)".to_string(),
        NodeKind::Unreadable(e) => format!(" (unreadable: {e})"),
        NodeKind::UnresolvedId(id) => format!(" (unresolved id: {id})"),
        NodeKind::AmbiguousAlias(name) => format!(" (ambiguous alias: [[{name}]])"),
        NodeKind::Foreign { workspace, id } => {
            format!(" (workspace {workspace}, id {id} — not followed)")
        }
    };
    println!("{connector}{name}{marker}");
    let child_prefix = if is_root {
        String::new()
    } else {
        format!("{prefix}{}", if is_last { "    " } else { "│   " })
    };
    for (i, child) in node.children.iter().enumerate() {
        print_node(child, &child_prefix, i + 1 == node.children.len(), false);
    }
}

/// One choice on an explore screen — what selecting the menu item does.
enum ExploreAction {
    /// Page the current document's raw text.
    View,
    /// Open the current document in `$EDITOR`.
    Edit,
    /// Navigate to another document (a resolved forward link or a backlink).
    Goto(PathBuf),
    /// A link that resolves to nothing followable (external, unresolved id,
    /// ambiguous alias) — selecting it just prints why.
    Note(String),
    /// Return to the previously-visited document.
    Back,
    Quit,
}

/// Interactively walk the workspace graph: at each document, view or edit it, or
/// follow any forward link (in any relation) or backlink to move on. A thin loop
/// over the library's resolution — the same path/id/alias resolution `tree` and
/// `check` use, with the reachability-scoped title index and the backlink map
/// each computed once up front.
fn cmd_explore(file: Option<&Path>) -> CmdResult {
    let ctx = find_root()?;
    let ws = workspace(&ctx)?;
    let root = ctx.root_doc.clone();
    let mut current = match file {
        Some(f) => ws_rel(&ctx, f)?,
        None => root.clone(),
    };
    // Alias resolution and backlinks, computed once — both bounded/lazy, so cheap
    // even at the root of a large repo.
    let titles = block_on(ws.title_index_scoped(&root))?;
    let backlinks = block_on(ws.backlinks(&root))?;

    let mut history: Vec<PathBuf> = Vec::new();
    loop {
        let full = ctx.root_dir.join(&current);
        let (text, doc) = match load(&full) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("prov: cannot open {}: {e}", current.display());
                match history.pop() {
                    Some(prev) => {
                        current = prev;
                        continue;
                    }
                    None => return Ok(ExitCode::FAILURE),
                }
            }
        };
        let title = doc
            .meta
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        // Build the menu: view/edit, every forward link (by relation), every
        // backlink, then navigation.
        let mut actions: Vec<(String, String, ExploreAction)> = Vec::new();
        actions.push((
            "View this document".into(),
            "page the raw file".into(),
            ExploreAction::View,
        ));
        actions.push(("Edit in $EDITOR".into(), String::new(), ExploreAction::Edit));

        // Documents already reachable from this screen by a forward link. A
        // backlink whose source is in this set is the inverse of a link we
        // already show — the child's `part_of` mirroring our `contents`, most
        // often — and navigates to the same place, so it is suppressed below to
        // keep a folder-note's menu from listing every child twice.
        let mut forward_targets: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::new();

        // Once per screen, not once per link: every foreign reference on this
        // document is answered against the same map, and a map edited between
        // screens is picked up on the next one.
        let peers = peer::PeerMap::load();
        for relation in ws.relations().relations() {
            let Some(value) = doc.meta.get(&relation.name) else {
                continue;
            };
            for raw in value.link_strings() {
                let parsed = link::Link::parse(&raw);
                let (label, action) = match ws.resolve_link_with(&current, &parsed, Some(&titles)) {
                    Target::Path(p) => {
                        let t = doc_title(&ctx, &p);
                        forward_targets.insert(p.clone());
                        (
                            format!("{}: {t}  ({})", relation.name, p.display()),
                            ExploreAction::Goto(p),
                        )
                    }
                    Target::External => (
                        format!("{}: {} (external)", relation.name, parsed.target),
                        ExploreAction::Note("external link — not followed".into()),
                    ),
                    Target::UnresolvedId(id) => (
                        format!("{}: {id} (unresolved id)", relation.name),
                        ExploreAction::Note("this id has no live registry entry".into()),
                    ),
                    Target::AmbiguousAlias(name) => (
                        format!("{}: {name} (ambiguous alias)", relation.name),
                        ExploreAction::Note("several documents share this title".into()),
                    ),
                    Target::Foreign { workspace, id } => (
                        format!("{}: {id} (workspace {workspace})", relation.name),
                        ExploreAction::Note(format!(
                            "another workspace — {}",
                            describe_peer(&peers.locate(&workspace), &workspace)
                        )),
                    ),
                };
                actions.push((label, String::new(), action));
            }
        }

        if let Some(inbound) = backlinks.get(&current) {
            for backlink in inbound {
                // Skip the inverse of a forward link already on this screen — the
                // same document, reached the same way (a child's `part_of` echoing
                // our `contents`). Genuinely-new backlinks (a `related` from a
                // document we don't link to) are unaffected.
                if forward_targets.contains(&backlink.source) {
                    continue;
                }
                let by = if backlink.by_id { "id" } else { "path" };
                actions.push((
                    format!("← {} [{}]", backlink.source.display(), backlink.site),
                    format!("linked from, by {by}"),
                    ExploreAction::Goto(backlink.source.clone()),
                ));
            }
        }

        if !history.is_empty() {
            actions.push((
                "Back".into(),
                "the previous document".into(),
                ExploreAction::Back,
            ));
        }
        actions.push(("Quit".into(), String::new(), ExploreAction::Quit));

        let header = if title.is_empty() {
            current.display().to_string()
        } else {
            format!("{} — {title}", current.display())
        };
        let mut menu = cliclack::select(header);
        for (i, (label, hint, _)) in actions.iter().enumerate() {
            menu = menu.item(i, label, hint);
        }
        // Any error (including a Ctrl-C / Esc cancel) leaves the explorer.
        let Ok(choice) = menu.interact() else { break };

        match &actions[choice].2 {
            ExploreAction::View => page_text(&text)?,
            ExploreAction::Edit => edit_file(&full)?,
            ExploreAction::Goto(p) => {
                history.push(current.clone());
                current = p.clone();
            }
            ExploreAction::Note(message) => eprintln!("prov: {message}"),
            ExploreAction::Back => {
                if let Some(prev) = history.pop() {
                    current = prev;
                }
            }
            ExploreAction::Quit => break,
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The title a linked document declares (its `title` frontmatter), else a title
/// derived from its filename — the label an explore menu shows for a link.
fn doc_title(ctx: &Ctx, rel: &Path) -> String {
    load(&ctx.root_dir.join(rel))
        .ok()
        .and_then(|(_, d)| {
            d.meta
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| link::path_to_title(rel))
}

/// Page `text` through `$PAGER` (default `less`), falling back to a plain print
/// when no pager can be spawned.
fn page_text(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".to_string());
    let mut parts = pager.split_whitespace();
    let Some(program) = parts.next() else {
        print!("{text}");
        return Ok(());
    };
    let spawned = std::process::Command::new(program)
        .args(parts)
        .stdin(std::process::Stdio::piped())
        .spawn();
    match spawned {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            child.wait()?;
        }
        Err(_) => print!("{text}"),
    }
    Ok(())
}

/// Open `path` in `$EDITOR`/`$VISUAL` (default `vi`), inheriting the terminal.
fn edit_file(path: &Path) -> std::io::Result<()> {
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    std::process::Command::new(program)
        .args(parts)
        .arg(path)
        .status()?;
    Ok(())
}

/// `check` — walk the workspace and report what is wrong with it.
///
/// `only` filters the *results*; it never narrows the walk. The findings that
/// matter most about a single document are the relational ones — nothing links
/// to it, its parent dropped it, an inbound label went stale — and every one of
/// those is discovered from somewhere else in the graph. Narrowing the walk to
/// reach them faster would be narrowing it past the evidence, and the command
/// would report a file clean because it could not see the three things wrong
/// with it. So `--only` costs a full check, and says so.
///
/// Narration to stderr; stdout carries the machine value — one finding per
/// line, or the whole set as a JSON array under `--json`.
fn cmd_check(
    root: Option<&Path>,
    fix: Option<FixModeArg>,
    only: Option<&Path>,
    as_json: bool,
) -> CmdResult {
    // `check` reports config issues in full (Finding::ConfigIssue), so skip the
    // one-line find_root warning that would just duplicate them.
    let mut ctx = find_root_quiet()?;
    // Heal first, validate second: if a mutation was interrupted by a crash, a
    // write-ahead journal is on disk. Roll it forward before reading the
    // workspace, so `check` reports on a consistent tree — and so the recovery
    // that `Error::Torn` points here to perform actually happens.
    match block_on(prov::recover(&StdFs, &ctx.root_dir))? {
        prov::Recovered::Applied(n) => {
            eprintln!("recovered an interrupted change: rolled {n} op(s) forward from the journal");
        }
        prov::Recovered::Nothing => {}
    }
    let root = match root {
        Some(r) => ws_rel(&ctx, r)?,
        None => ctx.root_doc.clone(),
    };
    // A subject that is not there would filter every finding away and report
    // the file clean — the one failure mode a per-file check must not have, and
    // an unreadable one at that, since "no findings" is exactly what a correct
    // run of this command usually prints. A typo is caught here instead.
    let only = only.map(|o| ws_rel(&ctx, o)).transpose()?;
    if let Some(subject) = &only
        && !ctx.root_dir.join(subject).exists()
    {
        return Err(format!(
            "{}: no such document — `--only` filters findings by subject, so a \
path that is not in the workspace would report clean",
            subject.display()
        )
        .into());
    }
    let mut ws = workspace(&ctx)?;
    let mut findings = block_on(ws.check(&root))?;
    // The generated page is checked alongside the graph, so "run `check` before
    // handing this workspace to someone" guarantees one more thing: that the
    // page describing it is not lying. Only when checking the workspace root —
    // a scoped `check <subtree>` is asking about that subtree.
    if root == ctx.root_doc {
        let about_ctx = about_context(&ctx)?;
        if let Some(finding) = block_on(ws.check_about(&ctx.root_doc, &ctx.config, &about_ctx))? {
            findings.push(finding);
        }
    }
    if let Some(subject) = &only {
        findings.retain(|f| f.subject() == subject);
    }
    let findings = findings;
    if let Some(mode) = fix {
        return cmd_check_fix(&mut ctx, &mut ws, &root, &findings, mode, only.as_deref());
    }
    if as_json {
        print!(
            "{}",
            json::J::Arr(findings.iter().map(json::finding).collect()).render()
        );
    } else {
        for finding in &findings {
            println!("{finding}");
        }
    }
    // The count line is narration for a person reading a terminal, and `--json`
    // is the mode where nobody is: the array says how many it holds, and says it
    // to the program that asked. Errors still go to stderr — this silences the
    // summary, not the diagnostics.
    if !as_json {
        let scope = match &only {
            Some(subject) => format!(" for {}", subject.display()),
            None => String::new(),
        };
        if findings.is_empty() {
            eprintln!("ok: no findings{scope}");
        } else {
            eprintln!("{} finding(s){scope}", findings.len());
        }
    }
    // Unchanged by `--json`: findings mean a non-zero exit, which is what lets
    // `prov check` stand as a CI gate. Worth knowing in a shell that treats a
    // non-zero exit as a failed pipeline (nushell does) — there, capture the
    // status rather than letting it abort the pipe:
    //
    //     (prov check --json | complete).stdout | from json
    if findings.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// Show, refresh or deeply verify the manifest covering a directory.
///
/// `target` is whichever handle the caller has — the covered directory, the
/// node describing it, or the manifest document itself — because after a
/// rename the three no longer share a name, and requiring the right one would
/// be requiring the user to know which.
///
/// Narration to stderr; stdout carries the machine value, which is the manifest
/// document's path (bare/`--update`) or one line per failing file (`--verify`).
fn cmd_manifest(target: &Path, update: bool, verify: bool) -> CmdResult {
    let ctx = find_root()?;
    let mut ws = workspace(&ctx)?;
    let target_rel = ws_rel(&ctx, target)?;
    let node = resolve_manifest_node(&ws, &target_rel)?;

    if update {
        let changed = block_on(ws.update_manifest(&node))?;
        persist(&ctx, &mut ws)?;
        if changed.is_clean() {
            eprintln!("{}: already up to date", changed.manifest.display());
        } else {
            eprintln!(
                "{}: {} added, {} removed, {} changed",
                changed.manifest.display(),
                changed.added.len(),
                changed.removed.len(),
                changed.changed.len()
            );
        }
        println!("{}", changed.manifest.display());
        return Ok(ExitCode::SUCCESS);
    }

    if verify {
        let findings = block_on(ws.verify_manifest(&node))?;
        for finding in &findings {
            println!("{finding}");
        }
        return if findings.is_empty() {
            eprintln!("ok: every listed file matches its checksum");
            Ok(ExitCode::SUCCESS)
        } else {
            eprintln!("{} file(s) no longer match their checksum", findings.len());
            Ok(ExitCode::FAILURE)
        };
    }

    let status = block_on(ws.manifest_status(&node))?
        .ok_or_else(|| format!("{} declares no manifest", node.display()))?;
    eprintln!(
        "{}: {} file(s) under {}{}",
        status.manifest.display(),
        status.listed,
        status.root.display(),
        if status.hashed {
            ", each with a checksum"
        } else {
            ", no checksums (an inventory)"
        }
    );
    for path in &status.missing {
        eprintln!("  missing: {}", path.display());
    }
    for path in &status.extra {
        eprintln!("  unlisted: {}", path.display());
    }
    if !status.agrees() {
        eprintln!("the directory has drifted — `prov manifest --update` records it as it is now");
    }
    println!("{}", status.manifest.display());
    Ok(if status.agrees() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The node describing `target`, which may be the covered directory, the node
/// itself, or the manifest document. A rename separates their names, so all
/// three are accepted rather than making the user work out which one prov wants.
fn resolve_manifest_node(
    ws: &Workspace<StdFs, Minter, FileIndex>,
    target: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    // A directory: the reverse lookup by convention.
    if let Ok(meta) = block_on(ws.graph().stat(target))
        && meta.is_dir()
    {
        return block_on(ws.manifest_node_covering(target))?.ok_or_else(|| {
            format!(
                "{} is not covered by a manifest — `prov attach --manifest {}` covers it",
                target.display(),
                target.display()
            )
            .into()
        });
    }
    // The node itself.
    if block_on(ws.manifest_of(target))?.is_some() {
        return Ok(target.to_path_buf());
    }
    // The manifest document: find the node through the directory it covers, so
    // the answer is the same one every other route gives.
    if let Ok(manifest) = block_on(ws.graph().read_manifest(target))
        && let Ok(root) = manifest.checked_root(target)
        && let Some(node) = block_on(ws.manifest_node_covering(&root))?
    {
        return Ok(node);
    }
    Err(format!(
        "{} is not a manifest, a manifest node, or a covered directory",
        target.display()
    )
    .into())
}

/// Repair the findings: walk them, and for each one offer everything
/// [`remedies`](prov::Workspace::remedies) can do about it.
///
/// A finding rarely has *one* repair, which is why this is a numbered menu and
/// not a yes/no. A broken link may be pointed at a near-match or dropped; a
/// contested containment may be settled either way; an orphan may be adopted
/// under any container above it. Prov ranks them but does not choose.
///
/// `remedies` is consulted lazily, one finding at a time, so a repair applied
/// early correctly changes — or empties — the offers a later finding makes.
///
/// Every line here is narration and goes to **stderr**; stdout stays reserved
/// for the machine value, which for this command is the findings a repair
/// *introduced*.
fn cmd_check_fix(
    ctx: &mut Ctx,
    ws: &mut Workspace<StdFs, Minter, FileIndex>,
    root: &Path,
    findings: &[prov::Finding],
    mode: FixModeArg,
    only: Option<&Path>,
) -> CmdResult {
    let mut applied = 0usize;
    let mut needs_attention = 0usize;
    // Choices the user asked to repeat, by remedy kind. Only ever consulted when
    // the finding at hand offers exactly one remedy of that kind — otherwise
    // "all of this kind" would silently pick between candidates it never saw.
    let mut repeat: BTreeSet<prov::RemedyKind> = BTreeSet::new();
    for finding in findings {
        let remedies = block_on(ws.remedies(finding))?;
        if remedies.is_empty() {
            eprintln!("•  {finding}");
            needs_attention += 1;
            continue;
        }

        // `mechanical` applies what restates an authority and nothing else. A
        // finding whose repairs all involve a choice is left standing, and
        // counted, so the exit code still reports it.
        if mode == FixModeArg::Mechanical {
            match remedies
                .iter()
                .find(|r| r.warrant == prov::Warrant::Derived)
            {
                Some(remedy) => {
                    eprintln!("⚑  {finding}");
                    eprintln!("   → {}", remedy.effect);
                    block_on(ws.apply_fix(&remedy.fix))?;
                    applied += 1;
                }
                None => {
                    eprintln!("•  {finding}");
                    needs_attention += 1;
                }
            }
            continue;
        }

        // A choice the user already made, repeatable only because it is
        // unambiguous here: exactly one remedy of that kind, and never a
        // destructive one.
        let repeated = repeat.iter().find_map(|kind| {
            let mut of_kind = remedies.iter().filter(|r| r.kind == *kind);
            let only = of_kind.next()?;
            (of_kind.next().is_none() && only.warrant != prov::Warrant::Destructive).then_some(only)
        });
        if let Some(remedy) = repeated {
            eprintln!("⚑  {finding}");
            eprintln!("   → {}", remedy.effect);
            block_on(ws.apply_fix(&remedy.fix))?;
            applied += 1;
            continue;
        }

        // One remedy is a yes/no question and reads better asked as one — which
        // is also what every finding looked like before findings could offer more
        // than one repair. Several is a menu.
        eprintln!("⚑  {finding}");
        let single = remedies.len() == 1;
        if single {
            eprintln!("   → {}  [{}]", remedies[0].effect, remedies[0].warrant);
        } else {
            for (n, remedy) in remedies.iter().enumerate() {
                eprintln!("   {}) {} [{}]", n + 1, remedy.effect, remedy.warrant);
            }
        }
        let answer = prompt(&if single {
            "   apply? [y]es / [n]o / [a]ll of this kind / [q]uit: ".to_string()
        } else {
            format!(
                "   [1-{}] / [s]kip / [a]ll of this kind / [q]uit: ",
                remedies.len()
            )
        })?;
        // A bare number picks; `a` picks the first and repeats that kind. `y` is
        // accepted only where there is nothing to disambiguate — with a menu on
        // screen, "yes" does not name an answer. EOF reads as an empty line, so a
        // non-interactive `--fix` skips everything rather than guessing;
        // `--fix mechanical` is the scriptable door.
        let chosen = match answer.as_str() {
            "y" | "yes" if single => Some(&remedies[0]),
            "q" | "quit" => {
                eprintln!("stopped; {applied} fix(es) applied");
                break;
            }
            "" | "s" | "skip" | "n" | "no" => None,
            "a" | "all" => {
                let first = &remedies[0];
                if first.warrant == prov::Warrant::Destructive {
                    // Never batch a removal, however emphatically it was asked
                    // for: the whole reason a link is reported rather than
                    // rewritten is that it records intent.
                    eprintln!("   (won't repeat a destructive repair — choose it one at a time)");
                    None
                } else {
                    repeat.insert(first.kind);
                    Some(first)
                }
            }
            other => match other.parse::<usize>() {
                Ok(n) if (1..=remedies.len()).contains(&n) => Some(&remedies[n - 1]),
                _ => None,
            },
        };
        match chosen {
            Some(remedy) => {
                block_on(ws.apply_fix(&remedy.fix))?;
                applied += 1;
            }
            None => needs_attention += 1,
        }
    }
    // A fix may have registered an ID (an adopted `id`, or an id-link back-link):
    // make sure a registry exists and persist the identity changes to disk. Gate
    // on the index actually having changed, so a purely path-based fix (a
    // path-style inverse, adopting an orphan by path) does not bootstrap an empty
    // registry document as a side effect.
    if applied > 0 && ws.index().is_dirty() {
        ensure_registry(ctx)?;
        persist(ctx, ws)?;
    }
    if applied == 0 {
        // Nothing ran, so a second walk would return what the first one did.
        eprintln!("applied 0 fix(es); {needs_attention} finding(s) need attention");
        return Ok(ExitCode::SUCCESS);
    }

    // Re-check and diff against the run these fixes were chosen from. A fix is a
    // *mutation of the graph*, so "applied N" is a report of effort, not of
    // outcome — and the count of what still needs attention was computed before
    // any of them ran. Only a second walk can say what actually changed, and only
    // the three buckets can separate what these fixes repaired from what they
    // broke from what was already wrong.
    let mut after = block_on(ws.check(root))?;
    // Scope the second walk the way the first one was scoped, or the diff would
    // compare a filtered before against an unfiltered after and read every
    // untouched finding elsewhere in the workspace as newly introduced.
    if let Some(subject) = only {
        after.retain(|f| f.subject() == subject);
    }
    let diff = prov::CheckDiff::between(findings, &after);

    for finding in &diff.introduced {
        println!("{finding}");
    }
    eprintln!(
        "applied {applied} fix(es): {} finding(s) resolved, {} introduced, {} still outstanding",
        diff.fixed.len(),
        diff.introduced.len(),
        diff.pre_existing.len()
    );
    if diff.is_clean() {
        return Ok(ExitCode::SUCCESS);
    }
    // A repair that broke something is the one outcome a script must not miss.
    // Outstanding findings on their own are not this run's verdict, and keep the
    // exit code they have always had.
    eprintln!("a fix introduced the finding(s) above — run `prov check` and review");
    Ok(ExitCode::FAILURE)
}

/// Prompt on stderr, read a trimmed, lowercased line from stdin (EOF → empty).
fn prompt(message: &str) -> Result<String, AnyError> {
    use std::io::Write;
    eprint!("{message}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_lowercase())
}

/// Print a route plan without applying it: what resolved, what is missing, and
/// where the missing nodes would land. Shared by `--dry-run` and the error a
/// missing route raises without `-p`, so the two describe the plan identically.
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
/// other document — a caller that mints must `ensure_registry` *before* this runs,
/// not merely before its own write.
fn resolve_placement(
    ctx: &Ctx,
    ws: &mut Workspace<StdFs, Minter, FileIndex>,
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
            return Ok(Some(ws_rel(ctx, &resolved)?));
        }
        TargetSpec::Route(route) => route,
    };
    let segments = Workspace::<StdFs>::route_segments(route);
    let plan = block_on(ws.plan_route(&ctx.root_doc, &segments, layout))?;
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
    let terminal = block_on(ws.apply_route(&plan))?;
    // Synthesized route parents are incidental to the command's result (the leaf),
    // so their creation is narration → stderr; the caller's stdout is the terminal.
    for synth in &plan.synthesize {
        eprintln!("created {} ({:?})", synth.path.display(), synth.title);
    }
    if created > 0 {
        persist(ctx, ws)?;
    }
    Ok(Some(terminal))
}

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
#[allow(clippy::too_many_arguments)]
fn cmd_new(
    title: &str,
    in_target: &str,
    parents: bool,
    layout: Layout,
    dry_run: bool,
    as_path: Option<&Path>,
    ext: Option<&str>,
) -> CmdResult {
    let mut ctx = find_root()?;
    // Authoring a reference that registers (the default style, or any relation's
    // override — e.g. `part_of: id` in a split) mints IDs, as does an eager
    // policy; ensure a registry to persist them exists *before* the workspace is
    // built over it. A route's synthesized nodes are `create`d too, so they mint
    // on the same terms — the registry has to exist before the route is applied,
    // not just before the leaf.
    let mints = ctx.config.mints_on_mutation();
    if mints && !dry_run {
        ensure_registry(&mut ctx)?;
    }

    // Resolve the parent. A path `--in` is already a path; a `@`-route walks the
    // tree from the root, and (with `-p`) creates what it doesn't find. Either way
    // the rest of this function is unchanged — a route is just another way to
    // *name* a parent, never a different kind of creation.
    let mut ws = workspace(&ctx)?;
    let Some(parent_rel) = resolve_placement(&ctx, &mut ws, in_target, parents, layout, dry_run)?
    else {
        return Ok(ExitCode::SUCCESS);
    };

    // The new document's path: an explicit `--as` wins; otherwise a readable
    // filename derived from the title — `slug(title).<ext>` beside the parent,
    // where the extension is `--ext` or the workspace's content format. The title
    // itself is always recorded in metadata (structure lives there, not the name).
    let path = match as_path {
        Some(p) => ws_rel(&ctx, p)?,
        None => {
            let extension = ext
                .map(str::to_owned)
                .unwrap_or_else(|| ctx.config.content_format.extension().to_string());
            let name = format!("{}.{extension}", link::slug(title));
            parent_rel.parent().unwrap_or(Path::new("")).join(name)
        }
    };
    // Leaf idempotency (`-p`): a target that already exists as the *same*
    // document (same title) is a no-op — `mkdir -p` for the leaf, completing the
    // route-parent `-p` above, so a daily-note cron can re-run the same command.
    // A path held by a *different*-titled document is a real collision and still
    // errors. Without `-p`, an existing leaf errors as before (via `create`).
    if parents && ws.fs_path(&path).exists() {
        let (_, existing) = load(&ws.fs_path(&path))?;
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
        block_on(ws.adopt(&path, &parent_rel))?;
        persist(&ctx, &mut ws)?;
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
    // (`ws` is the one built above — reusing it keeps any IDs a route just minted
    // in the same in-memory index this create registers into.)
    let created = block_on(ws.create_with_title(&path, &parent_rel, title))?;
    persist(&ctx, &mut ws)?;
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

/// Attach an arbitrary file — or, with `--all`, every loose file under the
/// workspace — minting a metadata sidecar and linking it under a parent
/// (default: the workspace root). Mirrors [`cmd_new`] — an id-registering
/// reference style or an eager policy mints IDs, so a registry is ensured first.
#[allow(clippy::too_many_arguments)]
fn cmd_attach(
    payload: Option<&Path>,
    in_target: Option<&str>,
    parents: bool,
    layout: Layout,
    opaque: bool,
    all: bool,
    recursive: bool,
    manifest: bool,
    hash: bool,
) -> CmdResult {
    let mut ctx = find_root()?;
    let mints = ctx.config.mints_on_mutation();
    if mints {
        ensure_registry(&mut ctx)?;
    }
    if recursive && !all {
        return Err("--recursive only applies with --all".into());
    }
    let mut ws = workspace(&ctx)?;
    // Default the parent to the workspace root — the common "attach this to my
    // workspace" case names no parent at all. Otherwise it is resolved exactly as
    // every other command resolves one, so an `@`-route `--in` works here too.
    let parent_rel = match in_target {
        None => ctx.root_doc.clone(),
        Some(t) => match resolve_placement(&ctx, &mut ws, t, parents, layout, false)? {
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
            block_on(ws.loose_attachments())?
        } else {
            block_on(ws.loose_attachments_in(&ctx.root_doc))?
        };
        if loose.is_empty() {
            eprintln!("no loose files to attach");
            return Ok(ExitCode::SUCCESS);
        }
        let mut attached = 0usize;
        for p in &loose {
            match block_on(ws.attach(p, &parent_rel)) {
                Ok(node) => {
                    eprintln!("attached {} (sidecar {})", p.display(), node.display());
                    println!("{}", node.display());
                    attached += 1;
                }
                Err(e) => eprintln!("prov: could not attach {}: {e}", p.display()),
            }
        }
        persist(&ctx, &mut ws)?;
        eprintln!("attached {attached} file(s) under {}", parent_rel.display());
        return Ok(ExitCode::SUCCESS);
    }

    let Some(payload) = payload else {
        return Err("specify a file to attach, or pass --all".into());
    };
    let payload_rel = ws_rel(&ctx, payload)?;

    // The bulk form: the positional is a directory, and it gains one node and
    // one list rather than a sidecar per file.
    if manifest {
        let node = block_on(ws.attach_manifest_titled(&payload_rel, &parent_rel, None, hash))?;
        persist(&ctx, &mut ws)?;
        let (manifest_doc, listed) = block_on(ws.manifest_of(&node))?
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
        block_on(ws.attach_opaque(&payload_rel, &parent_rel))?
    } else {
        block_on(ws.attach(&payload_rel, &parent_rel))?
    };
    persist(&ctx, &mut ws)?;
    eprintln!(
        "attached {} (sidecar {} in {})",
        payload.display(),
        node.display(),
        parent_rel.display()
    );
    println!("{}", node.display());
    Ok(ExitCode::SUCCESS)
}

fn cmd_mv(
    from: &str,
    to: &Path,
    in_target: Option<&str>,
    parents: bool,
    layout: Layout,
) -> CmdResult {
    let from_resolved = resolve_target(from)?;
    let mut ctx = find_root()?;
    // `rename` mints nothing, but `--under -p` synthesizes nodes with `create`,
    // which does — so a registry has to exist before the route runs, exactly as in
    // `new`/`reparent`. Plain `mv` skips this and stays as cheap as it was.
    if in_target.is_some() {
        let mints = ctx.config.mints_on_mutation();
        if mints {
            ensure_registry(&mut ctx)?;
        }
    }
    let mut ws = workspace(&ctx)?;
    let to_rel = ws_rel(&ctx, to)?;
    block_on(ws.rename(&ws_rel(&ctx, &from_resolved)?, &to_rel))?;
    eprintln!("moved {} -> {}", from_resolved.display(), to.display());

    // The move first, then the reparent — in that order because `rename` has
    // already retargeted every inbound link, so the parent the reparent removes is
    // found at the document's *new* path. Doing it the other way would reparent a
    // path that is about to stop existing.
    if let Some(target) = in_target {
        let Some(parent_rel) = resolve_placement(&ctx, &mut ws, target, parents, layout, false)?
        else {
            return Ok(ExitCode::SUCCESS);
        };
        if block_on(ws.reparent(&to_rel, &parent_rel))? != prov::Reparented::Unchanged {
            eprintln!("reparented {} -> in {}", to.display(), parent_rel.display());
        }
    }
    persist(&ctx, &mut ws)?;
    // The document's new location is the handle a caller acts on next.
    println!("{}", to_rel.display());
    Ok(ExitCode::SUCCESS)
}

fn cmd_reparent(
    path: &str,
    in_target: &str,
    parents: bool,
    layout: Layout,
    dry_run: bool,
) -> CmdResult {
    let mut ctx = find_root()?;
    // A route's synthesized nodes are `create`d and so mint on the same terms as
    // any other document — the registry must exist before the route is applied.
    // (The reparent itself authors links too, which an id-authoring workspace
    // registers.)
    let mints = ctx.config.mints_on_mutation();
    if mints && !dry_run {
        ensure_registry(&mut ctx)?;
    }

    let mut ws = workspace(&ctx)?;
    let Some(parent_rel) = resolve_placement(&ctx, &mut ws, in_target, parents, layout, dry_run)?
    else {
        return Ok(ExitCode::SUCCESS);
    };
    let path_rel = ws_rel(&ctx, &resolve_target(path)?)?;
    let outcome = block_on(ws.reparent(&path_rel, &parent_rel))?;
    persist(&ctx, &mut ws)?;
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

fn cmd_rm(path: &str, force: bool, purge: bool) -> CmdResult {
    let resolved = resolve_target(path)?;
    let ctx = find_root()?;
    let mut ws = workspace(&ctx)?;
    let target = ws_rel(&ctx, &resolved)?;

    // The safe default — move to the recycle bin — unless the workspace opted out
    // (`recycle_bin: false`) or the caller asked for a hard delete (`--purge`).
    let danglers = if ctx.config.recycle_bin && !purge {
        let danglers = block_on(ws.recycle(&target, force, None))?;
        persist(&ctx, &mut ws)?;
        println!(
            "moved {} to the recycle bin (restore with `prov restore`)",
            resolved.display()
        );
        danglers
    } else {
        let danglers = block_on(ws.delete(&target, force))?;
        persist(&ctx, &mut ws)?;
        println!("deleted {}", resolved.display());
        danglers
    };
    // The first recycle *bootstraps* the bin and adds the root's `recycle_bin`
    // pointer — another machinery file the page lists. A no-op on every later
    // delete, since the pointer already exists.
    refresh_about(&ctx.root_dir)?;
    for finding in &danglers {
        eprintln!("warning: now dangling — {finding}");
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_restore(path: &str) -> CmdResult {
    let ctx = find_root()?;
    let mut ws = workspace(&ctx)?;
    // The document is deleted, so its path cannot be `resolve_target`-ed (that
    // reads the file); take it as given, relative to the workspace root.
    let from = ws_rel(&ctx, Path::new(path))?;
    block_on(ws.restore(&from, &ctx.root_doc))?;
    persist(&ctx, &mut ws)?;
    eprintln!("restored {}", from.display());
    println!("{}", from.display());
    Ok(ExitCode::SUCCESS)
}

fn cmd_empty_bin() -> CmdResult {
    let ctx = find_root()?;
    let mut ws = workspace(&ctx)?;
    let purged = block_on(ws.empty_bin(&ctx.root_doc))?;
    persist(&ctx, &mut ws)?;
    // A bulk purge yields no object to name — narration only, stdout stays empty.
    eprintln!("purged {purged} document(s) from the recycle bin");
    Ok(ExitCode::SUCCESS)
}

/// Build the [`AboutContext`] for this workspace — the root's name and its
/// resolved pointer targets, which is everything the generator needs that is not
/// already in the config.
fn about_context(ctx: &Ctx) -> Result<prov::AboutContext, AnyError> {
    let probe: Workspace<StdFs> = Workspace::builder(StdFs)
        .root(&ctx.root_dir)
        .relations(ctx.config.relation_set())
        .build();
    Ok(prov::AboutContext {
        root_doc: ctx.root_doc.clone(),
        config_doc: block_on(probe.config_path(&ctx.root_doc))?,
        registry_doc: block_on(probe.registry_path(&ctx.root_doc))?,
        recycle_doc: block_on(probe.recycle_bin_path(&ctx.root_doc))?,
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
fn cmd_about(check: bool, print: bool) -> CmdResult {
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

/// `prov peer` — inspect and edit this device's map of other workspaces.
///
/// Deliberately does **not** need a workspace root: the map is a property of the
/// machine, and a user setting one up has often not `cd`'d anywhere in
/// particular. `peer resolve` is the one action that opens a workspace, and the
/// one it opens is the *peer*, never the current directory.
fn cmd_peer(action: PeerAction) -> CmdResult {
    match action {
        PeerAction::List => {
            let Some(file) = peer::path() else {
                println!("(no peer map)");
                eprintln!(
                    "no peer-map location for this invocation — no config directory could be \
                     determined.\n\
                     \n  Set one with --peers <FILE> or PROV_PEERS. Cross-workspace references \
                     work either way;\n  without a map they are carried but cannot be followed."
                );
                return Ok(ExitCode::SUCCESS);
            };
            let peers = peer::load();
            // The entries to stdout and the commentary to stderr, so `prov peer
            // list` pipes cleanly — the convention the other commands follow.
            for (name, root) in &peers {
                println!("{name}\t{}", root.display());
            }
            if peers.is_empty() {
                eprintln!(
                    "no peers recorded ({})\n\
                     \n  Add one with `prov peer add <name> <dir>`, where <name> is what that\n  \
                     workspace calls itself (`prov config workspace_id` there).",
                    file.display()
                );
            } else {
                eprintln!("{} peer(s) — {}", peers.len(), file.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        PeerAction::Add { name, dir } => {
            if !prov::is_valid_workspace_id(&name) {
                return Err(format!(
                    "`{name}` is not a valid workspace name — it cannot be empty or contain \
                     `/`, `:` or whitespace"
                )
                .into());
            }
            // Absolute, so the map means the same thing from every directory the
            // CLI is later run in. A peer map full of relative paths would
            // resolve differently per invocation, which is exactly the failure
            // mode that keeps it out of `prov.yaml` in the first place.
            let dir = dir
                .canonicalize()
                .map_err(|e| format!("{}: {e}", dir.display()))?;
            // Discovering the peer's root is what turns "a directory" into "a
            // workspace", and it is the first chance to notice that the name
            // being recorded is not the name that workspace answers to. It is no
            // longer the *last* chance — `peer resolve` asks again at the moment
            // it matters, because a line true when it was written can be stale by
            // the time it is followed — so this is advice, given early, and the
            // entry is recorded either way.
            let location = prov::PeerLocation::Path(dir.clone());
            match find_root_quiet_at(&dir) {
                Ok(peer_ctx) => {
                    // The same constructor the resolver uses, so `add` and
                    // `resolve` cannot come to different conclusions about the
                    // same directory.
                    match prov::PeerLookup::confirm(&name, location, &peer_ctx.config.workspace_id)
                    {
                        prov::PeerLookup::Confirmed(_) => {}
                        prov::PeerLookup::Unconfirmed { .. } => eprintln!(
                            "warning: the workspace at {} does not name itself — set \
                             `workspace_id` there\n  (`prov -C {} config workspace_id {name}`), \
                             or references written `id:{name}/<id>` will not be recognized as \
                             local when read inside it",
                            dir.display(),
                            dir.display()
                        ),
                        prov::PeerLookup::Mismatched { declares, .. } => eprintln!(
                            "warning: the workspace at {} calls itself `{declares}`, not \
                             `{name}` — references to it will be written `id:{declares}/<id>`, \
                             and `prov peer resolve id:{name}/<id>` will refuse this entry \
                             rather than follow it",
                            dir.display()
                        ),
                        prov::PeerLookup::Unknown => {
                            unreachable!("confirm never answers Unknown — it is given a location")
                        }
                    }
                }
                Err(e) => {
                    // Recorded anyway: a peer that is not a workspace *yet* is a
                    // reasonable thing to write down, and refusing would make the
                    // order of setup steps load-bearing.
                    eprintln!("warning: {}: {e}", dir.display());
                }
            }
            let mut peers = peer::load();
            let previous = peers.insert(name.clone(), dir.clone());
            peer::store(&peers)?;
            match previous {
                Some(old) if old != dir => {
                    eprintln!("{name} → {} (was {})", dir.display(), old.display())
                }
                _ => eprintln!("{name} → {}", dir.display()),
            }
            Ok(ExitCode::SUCCESS)
        }
        PeerAction::Remove { name } => {
            let mut peers = peer::load();
            if peers.remove(&name).is_none() {
                eprintln!("no peer named `{name}`");
                return Ok(ExitCode::FAILURE);
            }
            peer::store(&peers)?;
            eprintln!("removed `{name}` — references to it are still carried, just not followable");
            Ok(ExitCode::SUCCESS)
        }
        PeerAction::Resolve {
            reference,
            unverified,
        } => cmd_peer_resolve(&reference, unverified),
    }
}

/// One line saying where a peer is, or why it is not somewhere prov will go.
///
/// Every case names the location it found, including the ones it refuses: a
/// reader told only "cannot follow" has no way to see that the entry points at
/// the workspace next door.
fn describe_peer(lookup: &prov::PeerLookup, workspace: &str) -> String {
    match lookup {
        prov::PeerLookup::Confirmed(location) => {
            format!("`{location}`, per this device's peer map")
        }
        prov::PeerLookup::Unconfirmed { location, why } => {
            format!("`{location}`, but {why} (`--unverified` to follow it anyway)")
        }
        prov::PeerLookup::Mismatched { location, declares } => format!(
            "the peer map says `{location}`, but that workspace calls itself \
             `{declares}` — not followed (`prov peer add {workspace} <dir>` to correct it)"
        ),
        prov::PeerLookup::Unknown => format!(
            "no peer named `{workspace}` on this device (`prov peer add {workspace} <dir>`)"
        ),
    }
}

/// `prov peer resolve` — turn `id:<workspace>/<id>` into a file on this device.
///
/// The whole cross-workspace design in one command: the library parsed the
/// reference and stopped at "workspace `notes`, id `ajp7eq`"; everything past
/// that point is this device's peer map plus the *peer's own* registry. Nothing
/// here consults the current workspace at all.
///
/// The map is checked here rather than trusted here. A line recorded when it was
/// true and stale by now points at a directory that is some *other* workspace,
/// and following it would print a path to real documents in the wrong archive —
/// a wrong answer that looks exactly like a right one. So the peer is asked what
/// it calls itself, and a disagreement stops the command.
fn cmd_peer_resolve(reference: &str, unverified: bool) -> CmdResult {
    // Tolerate a bare `notes/ajp7eq` as well as the written `id:notes/ajp7eq`,
    // since the former is what a person reads off a screen.
    let written = if prov::link::strip_id_scheme(reference).is_some() {
        reference.to_string()
    } else {
        format!("{}{reference}", prov::link::ID_SCHEME)
    };
    let Some((peer_name, id)) = link::Link::parse(&written).foreign_target() else {
        return Err(format!(
            "`{reference}` is not a cross-workspace reference — expected `<workspace>/<id>`"
        )
        .into());
    };
    match peer::PeerMap::load().resolve_document(&peer_name, &id, unverified) {
        Ok(path) => {
            println!("{}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        Err(peer::DocumentError::Unfollowable(lookup)) => {
            Err(describe_peer(&lookup, &peer_name).into())
        }
        Err(peer::DocumentError::Unopenable(root, why)) => {
            Err(format!("{}: {why}", root.display()).into())
        }
        Err(peer::DocumentError::Unregistered(root)) => Err(format!(
            "`{id}` is not registered in the workspace at {}",
            root.display()
        )
        .into()),
    }
}

/// List what a tool copying, syncing or recording this folder should leave
/// alone: the difference between what is on disk and what the graph reaches.
///
/// Gated on nothing. Asking what the workspace fails to reach is a question
/// about the workspace, and the command writes nothing — the tool that
/// consumes the list owns every decision after this one.
///
/// The rules go to stdout (they are an ignore file's content, for `>` and
/// `diff` and friends); the count said *about* them goes to stderr.
fn cmd_ignore(why: bool, as_json: bool) -> CmdResult {
    let ctx = find_root()?;
    let ws = workspace(&ctx)?;
    let list = block_on(ws.ignore_list(&ctx.root_doc))?;

    if as_json {
        print!(
            "{}",
            json::J::Arr(list.rules.iter().map(json::ignore).collect()).render()
        );
        return Ok(ExitCode::SUCCESS);
    }
    // With `--why` the rules are grouped under a comment naming their reason,
    // rather than annotated one by one: gitignore reads `#` only at the start
    // of a line, so a trailing note would become part of the pattern and the
    // file would stop meaning what it says.
    let mut ordered: Vec<_> = list.rules.iter().collect();
    if why {
        ordered.sort_by_key(|rule| (rule.reason, rule.path.clone()));
    }
    let mut said: Option<prov::Reason> = None;
    for rule in ordered {
        if why && said != Some(rule.reason) {
            if said.is_some() {
                println!();
            }
            println!("# {}", reason_word(rule.reason));
            said = Some(rule.reason);
        }
        println!("{rule}");
    }
    // The count is narration for a person reading a terminal; `--json` is the
    // mode where nobody is, and it has already returned.
    match list.is_empty() {
        true => eprintln!("nothing to ignore — the graph reaches everything on disk"),
        false => eprintln!("{} rule(s)", list.rules.len()),
    }
    Ok(ExitCode::SUCCESS)
}

/// The one-word spelling of a reason, shared by `--why` and `--json` so the
/// two never drift into different vocabularies for the same fact.
pub(crate) fn reason_word(reason: prov::Reason) -> &'static str {
    match reason {
        prov::Reason::Bookkeeping => "bookkeeping",
        prov::Reason::Claimed => "claimed by a manifest",
        prov::Reason::Declared => "declared out of scope",
        prov::Reason::Hidden => "hidden",
        prov::Reason::Unreached => "unreached",
    }
}

/// Report a convert sweep: the changed document paths to stdout (one per line,
/// for `| git add` and friends), the human count to stderr.
fn report_converted(changed: &[PathBuf], target: &str) {
    for path in changed {
        println!("{}", path.display());
    }
    eprintln!("converted {} document(s) to {target}", changed.len());
}

fn cmd_convert(file: &Path, axis: &str, value: &str, recursive: bool, force: bool) -> CmdResult {
    let ctx = find_root()?;
    let mut ws = workspace(&ctx)?;
    // Convert authors path links in a target [`LinkStyle`], which fuses the
    // notation (bracketed/bare) and path resolution. Each axis composes with the
    // workspace's current *other* axis; `wikilink` has no path rendering to
    // convert, so it is rejected here.
    match axis {
        "path_style" | "path-style" => {
            let ps = PathStyle::from_config_str(value)
                .ok_or_else(|| format!("unknown path_style `{value}` (expected root|relative)"))?;
            let style = LinkStyle::from_axes(ctx.config.notation, ps);
            let changed = block_on(ws.convert_link_style(&ws_rel(&ctx, file)?, style, recursive))?;
            persist(&ctx, &mut ws)?;
            report_converted(&changed, &format!("{value} path resolution"));
        }
        "notation" => {
            let nt = Notation::from_config_str(value)
                .ok_or_else(|| format!("unknown notation `{value}` (expected markdown|bare)"))?;
            if nt == Notation::Wikilink {
                return Err("convert: `wikilink` has no path rendering to convert".into());
            }
            let style = LinkStyle::from_axes(nt, ctx.config.path_style);
            let changed = block_on(ws.convert_link_style(&ws_rel(&ctx, file)?, style, recursive))?;
            persist(&ctx, &mut ws)?;
            report_converted(&changed, &format!("{value} notation"));
        }
        "metadata.format" | "metadata_format" | "format" => {
            let fmt = prov::metadata_format_from_str(value).ok_or_else(|| {
                format!("unknown metadata.format `{value}` (expected yaml|json|toml|fig)")
            })?;
            let changed = block_on(ws.convert_meta_format(&ws_rel(&ctx, file)?, fmt, recursive))?;
            persist(&ctx, &mut ws)?;
            report_converted(&changed, &format!("{value} frontmatter"));
        }
        "metadata.embed" | "metadata_embed" | "embed" => {
            let style = EmbedStyle::from_config_str(value).ok_or_else(|| {
                format!(
                    "unknown metadata.embed `{value}` \
                     (expected delimited|code_block|html_script|html_code)"
                )
            })?;
            let changed = block_on(ws.convert_meta_embed(&ws_rel(&ctx, file)?, style, recursive))?;
            persist(&ctx, &mut ws)?;
            report_converted(&changed, &format!("{value} embedding"));
        }
        "content_format" | "content-format" | "content" => {
            let fmt = prov::ContentFormat::from_config_str(value).ok_or_else(|| {
                format!("unknown content_format `{value}` (expected markdown|djot|html)")
            })?;
            let changed =
                block_on(ws.convert_content_format(&ws_rel(&ctx, file)?, fmt, recursive, force))?;
            persist(&ctx, &mut ws)?;
            // Unlike the other axes, this one moves the files it converts — the
            // paths reported are where each document now *is*, not where it was.
            report_converted(&changed, &format!("{value} prose"));
        }
        other => {
            return Err(format!(
                "convert: axis `{other}` is not supported (only `notation`, `path_style`, \
                 `metadata.format`, `metadata.embed`, and `content_format`)"
            )
            .into());
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_duplicate(source: &str) -> CmdResult {
    let resolved = resolve_target(source)?;
    let mut ctx = find_root()?;
    // Attaching the copy authors the parent's spanning entry, which mints an ID
    // when that style registers (or under an eager policy) — same as `new`, so
    // bootstrap a registry to persist it before building the workspace.
    let mints = ctx.config.mints_on_mutation();
    if mints {
        ensure_registry(&mut ctx)?;
    }
    let mut ws = workspace(&ctx)?;
    let copy = block_on(ws.duplicate(&ws_rel(&ctx, &resolved)?))?;
    persist(&ctx, &mut ws)?;
    eprintln!("duplicated {} -> {}", resolved.display(), copy.display());
    println!("{}", copy.display());
    Ok(ExitCode::SUCCESS)
}

fn cmd_id(file: &Path) -> CmdResult {
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
fn cmd_id_workspace(requested: Option<&str>) -> CmdResult {
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

/// Look up a dotted key (`references.notation`) in a nested config mapping,
/// descending one mapping per segment.
fn lookup_dotted<'a>(map: &'a Mapping, dotted: &str) -> Option<&'a Value> {
    let mut segments = dotted.split('.');
    let mut current = map.get(segments.next()?)?;
    for seg in segments {
        current = current.get(seg)?;
    }
    Some(current)
}

/// Build the nested probe a dotted `config <key> <value>` implies, so `diagnose`
/// validates `references.notation=wikilink` as the nested shape it understands
/// rather than reading `references.notation` as one unknown top-level key.
fn nest_probe(dotted: &str, value: Value) -> Value {
    let mut node = value;
    for key in dotted.rsplit('.') {
        let mut m = Mapping::new();
        m.insert(key.to_string(), node);
        node = Value::Mapping(m);
    }
    node
}

/// Materialize the full effective config explicitly into the config document:
/// every setting written at its current-or-default value, so a workspace never
/// relies on invisible defaults. Bootstraps `prov.yaml` if none is linked,
/// preserves the document's own fields (title/part_of and any user fields) and
/// every setting already present (those are already in the effective config),
/// and fills in the rest. Canonicalizes layout (comments in the config document
/// are not preserved).
fn cmd_config_setup(mut ctx: Ctx) -> CmdResult {
    let config_doc = ensure_config(&mut ctx)?;
    let full = ctx.root_dir.join(&config_doc);
    let text = std::fs::read_to_string(&full)?;
    let doc = Document::parse(&config_doc, &text)?;
    let policy = ctx.config.to_mapping();
    // Keep the document's non-policy fields (title, part_of, user fields) in
    // place, then write every effective policy key explicitly after them.
    let mut map = Mapping::new();
    if let Some(existing) = doc.meta.as_mapping() {
        for (k, v) in existing {
            if !policy.contains_key(k) {
                map.insert(k.clone(), v.clone());
            }
        }
    }
    let count = policy.len();
    for (k, v) in policy {
        map.insert(k, v);
    }
    std::fs::write(
        &full,
        meta::serialize_mapping(&map, ctx.config.default_embed_format)?,
    )?;
    println!(
        "wrote {count} explicit setting(s) to {}",
        config_doc.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// Relocate the workspace's declared policy to a single home (`config --home`).
/// A *move*, not a materialization: only the *recognized policy* keys declared
/// across the two surfaces travel — no defaults baked in, and user fields stay
/// put — so the effective config is unchanged, just consolidated. Reads span both
/// homes regardless (`Ctx`); this only chooses where the bytes live.
fn cmd_config_home(mut ctx: Ctx, home: ConfigHome) -> CmdResult {
    // The recognized policy vocabulary: the keys `WorkspaceConfig` round-trips.
    // Anything else in a surface (a user field, a stray note) is not policy and
    // must not travel — so it is what the move ignores and what the delete guards.
    let recognized: std::collections::HashSet<String> =
        ctx.config.to_mapping().keys().cloned().collect();
    let declared = collect_declared_policy(&ctx, &recognized)?;
    match home {
        ConfigHome::Sidecar => move_policy_to_sidecar(&mut ctx, &declared, &recognized),
        ConfigHome::Root => move_policy_to_root(&mut ctx, &declared, &recognized),
    }
}

type KeySet = std::collections::HashSet<String>;

/// The recognized policy declared across both homes — the root's inline `prov:`
/// block with the sidecar's policy overlaid (the effective precedence *config
/// document > root block*), filtered to `recognized` so only policy travels.
/// Deep-merged, so a nested block present in both (e.g. `references`) combines
/// key-by-key rather than one home's block wholesale replacing the other's —
/// matching how `WorkspaceConfig::apply` layers.
fn collect_declared_policy(ctx: &Ctx, recognized: &KeySet) -> Result<Mapping, AnyError> {
    let mut merged = Mapping::new();
    let root_full = ctx.root_dir.join(&ctx.root_doc);
    if let Ok(text) = std::fs::read_to_string(&root_full)
        && let Ok(doc) = Document::parse(&ctx.root_doc, &text)
        && let Some(Value::Mapping(block)) = doc.meta.get(prov::config::ROOT_CONFIG_KEY)
    {
        merged = filter_keys(block, recognized);
    }
    let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&ctx.root_dir).build();
    if let Some(config_doc) = block_on(probe.config_path(&ctx.root_doc))? {
        let full = ctx.root_dir.join(&config_doc);
        let text = std::fs::read_to_string(&full)?;
        let doc = Document::parse(&config_doc, &text)?;
        if let Some(map) = doc.meta.as_mapping() {
            deep_merge(&mut merged, &filter_keys(map, recognized));
        }
    }
    Ok(merged)
}

/// A copy of `map` keeping only top-level keys in `keys`.
fn filter_keys(map: &Mapping, keys: &KeySet) -> Mapping {
    map.iter()
        .filter(|(k, _)| keys.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// A copy of `map` dropping the top-level keys in `keys`.
fn drop_keys(map: &Mapping, keys: &KeySet) -> Mapping {
    map.iter()
        .filter(|(k, _)| !keys.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Recursively overlay `overlay` onto `base`: a mapping-valued key present in both
/// merges key-by-key; every other key is replaced. The deep counterpart of
/// `Mapping::extend`, so `references: { notation }` in one home and
/// `references: { target }` in the other combine rather than clobber.
fn deep_merge(base: &mut Mapping, overlay: &Mapping) {
    for (key, value) in overlay {
        match (base.get_mut(key), value) {
            (Some(Value::Mapping(base_inner)), Value::Mapping(overlay_inner)) => {
                deep_merge(base_inner, overlay_inner);
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Rewrite the root's `prov:` block to `keep` its non-policy fields only (the
/// recognized policy having moved out): if nothing remains, remove the `prov:`
/// key entirely; otherwise set it to the remainder. The root's body and all other
/// fields are preserved.
fn strip_root_policy(ctx: &Ctx, recognized: &KeySet) -> Result<(), AnyError> {
    let root_full = ctx.root_dir.join(&ctx.root_doc);
    let text = std::fs::read_to_string(&root_full)?;
    let doc = Document::parse(&ctx.root_doc, &text)?;
    let Some(Value::Mapping(block)) = doc.meta.get(prov::config::ROOT_CONFIG_KEY) else {
        return Ok(());
    };
    let remainder = drop_keys(block, recognized);
    let updated = if remainder.is_empty() {
        edit::unset_in_text(&text, doc.carrier, prov::config::ROOT_CONFIG_KEY)?
    } else {
        edit::set_meta_in_text(
            &text,
            doc.carrier,
            prov::config::ROOT_CONFIG_KEY,
            &Value::Mapping(remainder),
        )?
    };
    std::fs::write(&root_full, updated)?;
    Ok(())
}

/// `config --home sidecar`: write the declared policy into `prov.yaml` (creating
/// and linking it if absent), preserving the sidecar's own `title`/`part_of` and
/// any non-policy fields, then strip the recognized policy from the root's `prov:`
/// block. Comments in the config document are not preserved (rebuilt canonically,
/// like `--setup`).
fn move_policy_to_sidecar(ctx: &mut Ctx, declared: &Mapping, recognized: &KeySet) -> CmdResult {
    let config_doc = ensure_config(ctx)?;
    let full = ctx.root_dir.join(&config_doc);
    let text = std::fs::read_to_string(&full)?;
    let doc = Document::parse(&config_doc, &text)?;
    // Keep every non-policy field the sidecar already has (title, part_of, and any
    // hand-added content), then write the policy after it.
    let mut map = doc
        .meta
        .as_mapping()
        .map(|m| drop_keys(m, recognized))
        .unwrap_or_default();
    for (k, v) in declared {
        map.insert(k.clone(), v.clone());
    }
    std::fs::write(
        &full,
        meta::serialize_mapping(&map, ctx.config.default_embed_format)?,
    )?;
    strip_root_policy(ctx, recognized)?;
    println!(
        "moved workspace policy to {} and cleared the root `prov:` block",
        config_doc.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// `config --home root`: merge the declared policy into the root's `prov:` block
/// (preserving any non-policy field already there), then retire the sidecar. If
/// stripping the policy leaves the sidecar with only its `title`/`part_of`, it is
/// deleted and its `config:` pointer removed; if it still carries hand-added
/// fields, it is kept (rewritten without the moved policy) so nothing is lost.
fn move_policy_to_root(ctx: &mut Ctx, declared: &Mapping, recognized: &KeySet) -> CmdResult {
    let root_full = ctx.root_dir.join(&ctx.root_doc);
    let text = std::fs::read_to_string(&root_full)?;
    let doc = Document::parse(&ctx.root_doc, &text)?;
    let mut block = match doc.meta.get(prov::config::ROOT_CONFIG_KEY) {
        Some(Value::Mapping(m)) => m.clone(),
        _ => Mapping::new(),
    };
    for (k, v) in declared {
        block.insert(k.clone(), v.clone());
    }
    let updated = edit::set_meta_in_text(
        &text,
        doc.carrier,
        prov::config::ROOT_CONFIG_KEY,
        &Value::Mapping(block),
    )?;
    std::fs::write(&root_full, updated)?;

    let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&ctx.root_dir).build();
    if let Some(config_doc) = block_on(probe.config_path(&ctx.root_doc))? {
        let sidecar_full = ctx.root_dir.join(&config_doc);
        let sidecar_text = std::fs::read_to_string(&sidecar_full)?;
        let sidecar = Document::parse(&config_doc, &sidecar_text)?;
        let remainder = sidecar
            .meta
            .as_mapping()
            .map(|m| drop_keys(m, recognized))
            .unwrap_or_default();
        let only_self_describing = remainder.keys().all(|k| k == "title" || k == "part_of");
        if only_self_describing {
            // The sidecar is now empty of meaning — remove its pointer and delete it.
            let text = std::fs::read_to_string(&root_full)?;
            let doc = Document::parse(&ctx.root_doc, &text)?;
            if doc.meta.get("config").is_some() {
                let updated = edit::unset_in_text(&text, doc.carrier, "config")?;
                std::fs::write(&root_full, updated)?;
            }
            std::fs::remove_file(&sidecar_full)?;
            println!(
                "moved workspace policy into the root `prov:` block and removed {}",
                config_doc.display()
            );
        } else {
            // Hand-added fields remain — keep the sidecar, just without the policy.
            std::fs::write(
                &sidecar_full,
                meta::serialize_mapping(&remainder, ctx.config.default_embed_format)?,
            )?;
            let kept: Vec<String> = remainder
                .keys()
                .filter(|k| k.as_str() != "title" && k.as_str() != "part_of")
                .cloned()
                .collect();
            println!(
                "moved workspace policy into the root `prov:` block; kept {} for its non-policy field(s): {}",
                config_doc.display(),
                kept.join(", ")
            );
        }
    } else {
        println!("moved workspace policy into the root `prov:` block");
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_config(
    key: Option<&str>,
    value: Option<&str>,
    setup: bool,
    home: Option<ConfigHome>,
) -> CmdResult {
    let ctx = find_root_quiet()?;
    // Both of these can change what the page says even though neither sets an
    // axis: `--home` moves policy between the two homes (and may delete or
    // create the sidecar the footer names), and `--setup` bootstraps a config
    // document where none was linked. The page names its config document, so
    // either one can leave it describing a file that is no longer there.
    if setup {
        let root_dir = ctx.root_dir.clone();
        let code = cmd_config_setup(ctx)?;
        refresh_about(&root_dir)?;
        return Ok(code);
    }
    if let Some(home) = home {
        let root_dir = ctx.root_dir.clone();
        let code = cmd_config_home(ctx, home)?;
        refresh_about(&root_dir)?;
        return Ok(code);
    }
    match (key, value) {
        // No key: print the effective config (defaults + root + config document).
        (None, _) => {
            print!(
                "{}",
                meta::serialize_mapping(&ctx.config.to_mapping(), Format::Yaml)?
            );
        }
        // Key only: read that value from the *effective* config (defaults + root
        // frontmatter + config document), so it agrees with the no-key form
        // above. Reading the config document alone would report "not set" for a
        // value that comes from root frontmatter (the diaryx-compat path) or
        // stands at its default — a divergence between the two forms.
        (Some(key), None) => {
            let effective = ctx.config.to_mapping();
            // Dotted keys address nested axes (`references.notation`).
            match lookup_dotted(&effective, key) {
                Some(v) => match v.as_str() {
                    Some(s) => println!("{s}"),
                    None => println!("{}", meta::serialize_value(v, Format::Yaml)?.trim_end()),
                },
                None => {
                    eprintln!("prov: {key} is not set");
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        // Key + value: materialize/link the config document if needed, then set.
        (Some(key), Some(value)) => {
            let mut ctx = ctx;
            // The scalar the text implies (`true` → a bool, `12` → an int),
            // carried as prov's own value from here on — what `diagnose` reads
            // and what the write emits, so the setting that is judged is exactly
            // the setting that lands.
            //
            // `workspace_id` is exempt because it is a *name*, and a name that
            // happens to be spelled with digits is still a name. The mint's
            // alphabet includes the digits (`prov id --workspace` can hand back
            // `123456789012`), so inferring an int here would have prov refuse
            // to set a name it had just minted itself.
            let inferred: Value = if key == "workspace_id" {
                Value::String(value.to_string())
            } else if key == "out_of_scope" {
                // The one sequence-valued axis reachable from here, so the one
                // that needs a spelling a shell can produce: comma-separated,
                // because the alternative is asking a person to hand-edit YAML
                // for the axis whose whole purpose is to be set once by someone
                // who just noticed another tool's folder beside their notes.
                // An empty value clears the list rather than declaring `""`.
                Value::Sequence(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|dir| !dir.is_empty())
                        .map(|dir| Value::String(dir.to_string()))
                        .collect(),
                )
            } else {
                edit::infer_scalar(value).into()
            };
            // Refuse to write a setting prov would silently ignore — the same
            // conditions `check` flags (a key that resembles a real axis but
            // isn't, or a recognized axis with an unrecognized value). Running the
            // shared diagnostic over a one-key probe keeps set-time and check-time
            // judgments identical. A truly novel key (resembling no axis) is left
            // to pass — it may be a user field or a forward-compatible key.
            let probe = nest_probe(key, inferred.clone());
            if let Some(issue) = prov::diagnose(&probe).into_iter().next() {
                match issue.kind {
                    prov::ConfigIssueKind::UnknownKey { suggestion } => {
                        eprintln!(
                            "prov: unknown config key `{key}` — did you mean `{suggestion}`?"
                        );
                    }
                    prov::ConfigIssueKind::InvalidValue { value, expected } => {
                        eprintln!(
                            "prov: `{value}` is not a valid {key} (expected: {})",
                            expected.join(", ")
                        );
                    }
                    prov::ConfigIssueKind::SpanningNotSingleParent { inverse } => {
                        eprintln!(
                            "prov: spanning relation's inverse `{inverse}` must be `cardinality: one` to form a single-parent tree"
                        );
                    }
                    prov::ConfigIssueKind::MalformedWorkspaceId { value } => {
                        eprintln!(
                            "prov: `{value}` is not a valid workspace name — it cannot be empty or contain `/`, `:` or whitespace"
                        );
                    }
                    // Not reachable from a one-key probe (this needs a `fields`
                    // declaration alongside the view), but spelled out rather
                    // than wildcarded so a new issue kind arrives here as a
                    // compile error.
                    prov::ConfigIssueKind::NestNotSingleValued { field } => {
                        eprintln!(
                            "prov: cannot nest by `{field}` — it is declared `type: seq`, and a document with several values has several homes"
                        );
                    }
                }
                return Ok(ExitCode::FAILURE);
            }
            let config_doc = write_config_setting(&mut ctx, key, inferred)?;
            eprintln!("set {key} = {value} in {}", config_doc.display());
            refresh_about(&ctx.root_dir)?;
            // Echo the value now in effect, so `v=$(prov config set …)` round-trips
            // with `prov config <key>`.
            println!("{value}");
        }
    }
    Ok(ExitCode::SUCCESS)
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
fn refresh_about(root_dir: &Path) -> Result<(), AnyError> {
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

/// Ensure the workspace *declares* a config document, bootstrapping one when it
/// does not: create `prov.<ext>` (in the workspace's metadata format) beside
/// the root (self-described with a title) and add the `config` pointer to the
/// root's metadata. Returns its path relative to the root. Mirrors
/// [`ensure_registry`], including its change set: a config document the root does
/// not point at is one nothing will ever read. Like the registry, it carries no
/// `part_of` — machinery is reached one-way through the root's pointer (DESIGN §5).
/// Write one setting into the workspace's config document, bootstrapping and
/// linking it first if the workspace declares none. Returns the config
/// document's path relative to the root, for the caller to narrate.
///
/// The write half of `prov config <key> <value>`, factored out so anything that
/// sets a single axis on the user's behalf (`prov id --workspace`) lands in the
/// same file, through the same editor, as if they had set it by hand. The
/// *validation* half stays with `config`: this takes a [`Value`] already decided
/// on, so a caller that knows the exact scalar it wants (a minted workspace
/// name, which must stay a string) is not forced back through scalar inference.
fn write_config_setting(ctx: &mut Ctx, key: &str, value: Value) -> Result<PathBuf, AnyError> {
    let config_doc = ensure_config(ctx)?;
    let full = ctx.root_dir.join(&config_doc);
    let text = std::fs::read_to_string(&full)?;
    let doc = Document::parse(&config_doc, &text)?;
    let updated = edit::set_in_text(&text, doc.carrier, key, (&value).into())?;
    std::fs::write(&full, updated)?;
    Ok(config_doc)
}

fn ensure_config(ctx: &mut Ctx) -> Result<PathBuf, AnyError> {
    let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&ctx.root_dir).build();
    if let Some(existing) = block_on(probe.config_path(&ctx.root_doc))? {
        return Ok(existing);
    }
    let format = ctx.config.default_embed_format;
    let config_rel = PathBuf::from(sidecar_name(CONFIG_STEM, format));

    let mut seed = Mapping::new();
    seed.insert("title".into(), Value::String("prov config".into()));

    let created =
        block_on(probe.link_sidecar(&ctx.root_doc, "config", &config_rel, &seed, format))?;
    if created {
        eprintln!(
            "initialized {} (linked from {})",
            config_rel.display(),
            ctx.root_doc.display()
        );
    }
    Ok(config_rel)
}

fn cmd_backlinks(file: &Path) -> CmdResult {
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

fn cmd_resolve(id: &str) -> CmdResult {
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

#[cfg(test)]
mod tests {
    use super::{now_rfc3339, rfc3339};

    #[test]
    fn rfc3339_matches_known_instants() {
        // Cross-checked against `date -u -r <secs>` / any RFC 3339 reference.
        assert_eq!(rfc3339(0, 0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(rfc3339(1_700_000_000, 0), "2023-11-14T22:13:20.000000Z");
        // A leap day, to exercise the calendar arithmetic.
        assert_eq!(rfc3339(1_582_934_400, 0), "2020-02-29T00:00:00.000000Z");
        // End-of-year boundary.
        assert_eq!(rfc3339(1_609_459_199, 0), "2020-12-31T23:59:59.000000Z");
    }

    #[test]
    fn the_fraction_is_six_digits_whatever_the_value() {
        // Fixed width is the whole point: sub-second precision exists so two
        // instants in the same second can be *ordered*, and a trimmed fraction
        // breaks that. A leading-zero microsecond count must not lose its zeros,
        // and a whole-number one must not lose its trailing ones.
        assert_eq!(rfc3339(0, 1), "1970-01-01T00:00:00.000001Z");
        assert_eq!(rfc3339(0, 100_000), "1970-01-01T00:00:00.100000Z");
        assert_eq!(rfc3339(0, 999_999), "1970-01-01T00:00:00.999999Z");

        // …and with that, a plain string comparison is a correct total order.
        let mut stamps = [rfc3339(0, 120_000), rfc3339(0, 100_000), rfc3339(0, 99_999)];
        stamps.sort();
        assert_eq!(
            stamps,
            [rfc3339(0, 99_999), rfc3339(0, 100_000), rfc3339(0, 120_000)]
        );
    }

    #[test]
    fn the_clock_is_microsecond_precise_and_never_goes_backwards() {
        // The one assertion about the real clock: the format it produces is the
        // one the store's ordering rests on.
        let now = now_rfc3339();
        assert_eq!(now.len(), "2026-07-16T14:30:00.123456Z".len(), "{now}");
        assert!(now.ends_with('Z') && now.as_bytes()[19] == b'.', "{now}");
        assert!(now_rfc3339() >= now, "the clock must not run backwards");
    }
}
