//! The session layer: where the workspace is, and how a command opens it.
//!
//! Every workspace command begins the same way — discover the root from the
//! current directory ([`find_root`]), build the library's [`Workspace`] over
//! it with the config's policy knobs applied ([`workspace`]), and, once a
//! mutation has run, land whatever identity changes it made ([`persist`]).
//! This module is that beginning and end, kept in one place so the commands
//! cannot disagree about what a workspace *is* — which registry it reads,
//! which documents are machinery rather than content, how a CLI argument
//! names a document ([`resolve_target`]).
//!
//! Nothing here prints a result or parses a flag: it is the plumbing between
//! the argument grammar in [`crate::cli`] and the library, and each command
//! module reaches it by name.

use std::path::{Path, PathBuf};

use prov::{
    ChangeSet, Document, FileIndex, Id, IdIndex, IdStorage, IndexStore, Layout, Mapping, Minter,
    Settings, StdFs, Value, Workspace, WorkspaceConfig, block_on, edit, link,
};

use crate::AnyError;
use crate::about::refresh_about;
use crate::cli::{REGISTRY_STEM, sidecar_name};

/// The discovered workspace context: where the root is, which document is the
/// root, and where the root says the registry lives.
pub(crate) struct Ctx {
    /// Absolute path of the workspace root directory.
    pub(crate) root_dir: PathBuf,
    /// The root document, relative to `root_dir`.
    pub(crate) root_doc: PathBuf,
    /// The registry document the root declares (relative to `root_dir`), if any.
    pub(crate) registry: Option<PathBuf>,
    /// The workspace node (`prov.yaml`) found by convention, relative to
    /// `root_dir`, if any — the one document a timestamp stamp must never land
    /// in, because its `updated` key is the *name* of the field and not a
    /// value of it.
    pub(crate) node: Option<PathBuf>,
    /// The effective workspace config (root frontmatter overlaid by the linked
    /// config document, over defaults).
    pub(crate) config: WorkspaceConfig,
}

impl From<prov::Discovered> for Ctx {
    fn from(d: prov::Discovered) -> Self {
        Ctx {
            root_dir: d.root_dir,
            root_doc: d.root_doc,
            registry: d.registry,
            node: d.node.node,
            config: d.config,
        }
    }
}

/// An open workspace: the discovered [`Ctx`] and the library engine built over
/// it, which is what every workspace command holds from its first line to its
/// last.
///
/// The two constructors are the two ways a command begins. [`Session::open`]
/// is a read, or a mutation that cannot mint an ID. [`Session::open_for_mutation`]
/// is a mutation that may — because the reference style registers, or the
/// identity policy is eager — and so makes sure a registry exists *before* the
/// workspace is built over it, since a route's synthesized nodes are `create`d
/// and mint on the same terms as the leaf. That precondition used to be a
/// comment at each call site; here it is one line, and a verb that opens the
/// wrong way is the only way to get it wrong.
///
/// [`Session::commit`] is the end: the identity changes a mutation made, landed
/// according to the workspace's storage mode.
pub(crate) struct Session {
    pub(crate) ctx: Ctx,
    pub(crate) ws: Workspace<StdFs, Minter, FileIndex>,
}

impl Session {
    /// The workspace around the current directory, for reading or for a
    /// mutation that mints nothing.
    pub(crate) fn open() -> Result<Self, AnyError> {
        Self::over(find_root()?)
    }

    /// The workspace around the current directory, for a mutation that may
    /// mint — a registry is bootstrapped first when the config says it does.
    pub(crate) fn open_for_mutation() -> Result<Self, AnyError> {
        Self::mutating(find_root()?)
    }

    /// [`Session::open`] over a context the caller already discovered.
    pub(crate) fn over(ctx: Ctx) -> Result<Self, AnyError> {
        let ws = workspace(&ctx)?;
        Ok(Self { ctx, ws })
    }

    /// [`Session::open_for_mutation`] over a context the caller already
    /// discovered.
    pub(crate) fn mutating(mut ctx: Ctx) -> Result<Self, AnyError> {
        if ctx.config.mints_on_mutation() {
            ensure_registry(&mut ctx)?;
        }
        Self::over(ctx)
    }

    /// Land the identity changes a mutation made — see [`persist`].
    pub(crate) fn commit(&mut self) -> Result<(), AnyError> {
        persist(&self.ctx, &mut self.ws)
    }
}

/// Resolve the workspace root and, on success, warn (once, to stderr) about any
/// config a command would otherwise run past silently — settings prov would
/// ignore, or a config `spec` newer than this build. Suppressed by
/// `PROV_QUIET`. Commands that already report config in full
/// (`check`, `config`) use [`find_root_quiet`] instead.
pub(crate) fn find_root() -> Result<Ctx, AnyError> {
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
pub(crate) fn find_root_quiet() -> Result<Ctx, AnyError> {
    let cwd = std::env::current_dir()?;
    find_root_quiet_at(&cwd)
}

/// [`find_root_quiet`], but discovering from `dir` rather than the process's
/// current directory — for a re-discovery after a write has changed the config
/// on disk, where the caller already knows the root.
pub(crate) fn find_root_quiet_at(dir: &Path) -> Result<Ctx, AnyError> {
    match block_on(prov::discover(&StdFs, dir))? {
        prov::Discovery::Found(d) => Ok(Ctx::from(d)),
        prov::Discovery::Ambiguous { dir, candidates } => Err(format!(
            "ambiguous workspace root in {}: {} (rename one, add part_of, or declare the workspace with a prov.yaml beside its root)",
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

/// The workspace around `dir`, if there is one — for a command that works on
/// a *file* and keeps working outside any workspace (`set`, `unset`), but does
/// the workspace's bookkeeping when it finds itself inside one. Every answer
/// other than a clean find is `None`: an ambiguous root is a fault `check` and
/// `edit` report, and a single-field edit is not the place to refuse over it.
pub(crate) fn workspace_around(dir: &Path) -> Option<Ctx> {
    match block_on(prov::discover(&StdFs, dir)) {
        Ok(prov::Discovery::Found(d)) => Some(Ctx::from(d)),
        _ => None,
    }
}

/// The documents that are the workspace's own record-keeping rather than its
/// content — the node, the registry, the deletion log, and each flat `fields`
/// vocabulary — as workspace-relative paths.
///
/// These are exactly the whole-file stores `check` holds to the whole-file
/// rule, plus the node, and they are enumerated here for the opposite reason:
/// a timestamp is a claim about *content*, and stamping one into a store
/// corrupts it. The node's `updated:` key names the field, so a stamp there
/// overwrites the name with a value; the registry is a record store prov
/// re-lays-out from its own shape. A reified vocabulary is content — an index
/// node with term documents under it — and is not listed.
pub(crate) fn machinery(
    ctx: &Ctx,
    ws: &Workspace<StdFs, Minter, FileIndex>,
) -> Result<Vec<PathBuf>, AnyError> {
    let mut stores: Vec<PathBuf> = ctx.node.iter().cloned().collect();
    stores.extend(ctx.registry.iter().cloned());
    stores.extend(block_on(ws.deletions_path(&ctx.root_doc))?);
    for (_, spec) in ctx.config.field_declarations() {
        if spec.reify {
            continue;
        }
        if let Some(pointer) = &spec.vocabulary
            && let Some(p) = ws.vocabulary_path(&ctx.root_doc, pointer)
        {
            stores.push(p);
        }
    }
    Ok(stores)
}

/// The timestamp half of a content change to `rel`: the workspace's `updated`
/// field and the instant `now`, or `None` when the workspace keeps no such
/// field or `rel` is [`machinery`] rather than content. Shared by
/// every verb that stamps — `edit`, `set`, `unset`, `stamp` — so they cannot
/// disagree about which documents a timestamp may land in.
pub(crate) fn updated_stamp<'a>(
    ctx: &'a Ctx,
    machinery: &[PathBuf],
    rel: &Path,
    now: &'a str,
) -> Option<(&'a str, &'a str)> {
    if ctx.config.updated.is_empty() || machinery.iter().any(|m| m == rel) {
        return None;
    }
    Some((ctx.config.updated.as_str(), now))
}

/// The workspace the multi-document commands drive: rooted at the discovered
/// root, a lazy identity policy, and the registry the root declares (an empty
/// in-memory one when the root declares none — see `ensure_registry`).
pub(crate) fn workspace(ctx: &Ctx) -> Result<Workspace<StdFs, Minter, FileIndex>, AnyError> {
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
pub(crate) fn ensure_registry(ctx: &mut Ctx) -> Result<(), AnyError> {
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
pub(crate) enum TargetSpec<'a> {
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
pub(crate) fn parse_target(s: &str) -> TargetSpec<'_> {
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
pub(crate) fn resolve_target(s: &str) -> Result<PathBuf, AnyError> {
    match parse_target(s) {
        TargetSpec::Path(p) => Ok(PathBuf::from(p)),
        TargetSpec::Id(id) => {
            let Session { ctx, ws } = Session::open()?;
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
            let Session { ctx, ws } = Session::open()?;
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
pub(crate) fn ws_rel(ctx: &Ctx, path: &Path) -> Result<PathBuf, AnyError> {
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
pub(crate) fn entropy_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::hash::RandomState::new().build_hasher().finish()
}

pub(crate) fn load(file: &Path) -> Result<(String, Document), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(file)?;
    let doc = Document::parse(file, &text)?;
    Ok((text, doc))
}
